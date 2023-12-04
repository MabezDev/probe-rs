//! Xtensa Debug Module Communication

// TODO: remove
#![allow(missing_docs)]

use crate::{
    architecture::xtensa::arch::{self, instruction},
    probe::JTAGAccess,
    DebugProbeError, MemoryInterface,
};

use super::xdm::{Error as XdmError, Xdm};

/// Possible Xtensa errors
#[derive(thiserror::Error, Debug)]
pub enum XtensaError {
    /// An error originating from the DebugProbe
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
    /// Xtensa debug module error
    #[error("Xtensa debug module error: {0}")]
    XdmError(XdmError),
}

#[derive(Debug)]
struct XtensaCommunicationInterfaceState {
    /// Pairs of (register, value). TODO: can/should we handle special registers?
    saved_registers: Vec<(u8, u32)>,
}

/// A interface that implements controls for Xtensa cores.
#[derive(Debug)]
pub struct XtensaCommunicationInterface {
    /// The Xtensa debug module
    xdm: Xdm,

    state: XtensaCommunicationInterfaceState,
}

impl XtensaCommunicationInterface {
    /// Create the Xtensa communication interface using the underlying probe driver
    pub fn new(probe: Box<dyn JTAGAccess>) -> Result<Self, (Box<dyn JTAGAccess>, DebugProbeError)> {
        let xdm = Xdm::new(probe).map_err(|(probe, e)| match e {
            XtensaError::DebugProbe(err) => (probe, err),
            other_error => (probe, DebugProbeError::Other(other_error.into())),
        })?;

        let s = Self {
            xdm,
            state: XtensaCommunicationInterfaceState {
                saved_registers: vec![],
            },
        };

        Ok(s)
    }

    pub fn enter_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.xdm.enter_ocd_mode()?;
        tracing::info!("Entered OCD mode");
        Ok(())
    }

    pub fn is_in_ocd_mode(&mut self) -> Result<bool, XtensaError> {
        self.xdm.is_in_ocd_mode()
    }

    pub fn leave_ocd_mode(&mut self) -> Result<(), XtensaError> {
        self.resume()?;
        self.xdm.leave_ocd_mode()?;
        tracing::info!("Left OCD mode");
        Ok(())
    }

    pub fn halt(&mut self) -> Result<(), XtensaError> {
        self.xdm.halt()?;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), XtensaError> {
        self.restore_registers()?;
        self.xdm.resume()?;

        Ok(())
    }

    // TODO make sure only A* registers are used as `register
    fn read_register(&mut self, register: u8) -> Result<u32, XtensaError> {
        let move_to_ddr = instruction::rsr(arch::SR_DDR, register);
        self.xdm.execute_instruction(move_to_ddr)?;
        let register = self.xdm.read_ddr()?;

        Ok(register)
    }

    fn write_register_impl(&mut self, register: u8, value: u32) -> Result<(), XtensaError> {
        self.xdm.write_ddr(value)?;
        self.xdm
            .execute_instruction(instruction::rsr(arch::SR_DDR, register))?;

        Ok(())
    }

    // TODO make sure only A* registers are used as `register`
    fn write_register(&mut self, register: u8, value: u32) -> Result<(), XtensaError> {
        if !self.is_register_saved(register) {
            self.save_register(register)?;
        }

        self.write_register_impl(register, value)
    }

    fn debug_execution_error(&mut self, status: XdmError) -> Result<(), XtensaError> {
        if let XdmError::ExecExeception = status {
            tracing::warn!("Failed to execute instruction, attempting to read debug info");

            // clear ExecException to allow new instructions to run
            self.xdm.clear_exec_exception()?;

            // dump EXCCAUSE, EXCVADDR and DEBUGCAUSE
            // we must not use `self.read_register` and `self.write_register` here
            // TODO we need to make sure our scratch register (A3) is saved properly

            for (name, reg) in [
                ("EXCCAUSE", arch::SR_EXCCAUSE),
                ("EXCVADDR", arch::SR_EXCVADDR),
                ("DEBUGCAUSE", arch::SR_DEBUGCAUSE),
            ] {
                self.xdm
                    .execute_instruction(instruction::rsr(reg, arch::A3))?;
                self.xdm
                    .execute_instruction(instruction::wsr(arch::A3, arch::SR_DDR))?;
                let register = self.xdm.read_ddr()?;

                tracing::info!("{}: {:08x}", name, register);
            }
        }

        Ok(())
    }

    fn execute_instruction(&mut self, inst: u32) -> Result<(), XtensaError> {
        let status = self.xdm.execute_instruction(inst);
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    fn read_ddr_and_execute(&mut self) -> Result<u32, XtensaError> {
        let status = self.xdm.read_ddr_and_execute();
        if let Err(XtensaError::XdmError(err)) = status {
            self.debug_execution_error(err)?
        }
        status
    }

    fn is_register_saved(&mut self, register: u8) -> bool {
        self.state
            .saved_registers
            .iter()
            .any(|(reg, _)| *reg == register)
    }

    fn save_register(&mut self, register: u8) -> Result<(), XtensaError> {
        let value = self.read_register(register)?;
        self.state.saved_registers.push((register, value));

        Ok(())
    }

    fn restore_registers(&mut self) -> Result<(), XtensaError> {
        for (register, value) in std::mem::take(&mut self.state.saved_registers) {
            self.write_register_impl(register, value)?;
        }

        Ok(())
    }
}

impl MemoryInterface for XtensaCommunicationInterface {
    fn read(&mut self, address: u64, dst: &mut [u8]) -> Result<(), crate::Error> {
        if dst.is_empty() {
            return Ok(());
        }

        let read_words = if dst.len() % 4 == 0 {
            dst.len() / 4
        } else {
            dst.len() / 4 + 1
        };

        // Write address to A3
        self.write_register(arch::A3, address as u32)?;

        // Read from address in A3
        self.execute_instruction(instruction::lddr32_p(arch::A3))?;

        for i in 0..read_words - 1 {
            let word = self.read_ddr_and_execute()?.to_le_bytes();
            dst[4 * i..][..4].copy_from_slice(&word);
        }

        let remaining_bytes = if dst.len() % 4 == 0 { 4 } else { dst.len() % 4 };

        let word = self.xdm.read_ddr()?;
        dst[4 * (read_words - 1)..][..remaining_bytes]
            .copy_from_slice(&word.to_le_bytes()[..remaining_bytes]);

        Ok(())
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut out = [0; 4];
        self.read(address, &mut out)?;

        Ok(u32::from_le_bytes(out))
    }

    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> anyhow::Result<u64, crate::Error> {
        todo!()
    }

    fn read_word_8(&mut self, address: u64) -> anyhow::Result<u8, crate::Error> {
        todo!()
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> anyhow::Result<(), crate::Error> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> anyhow::Result<bool, crate::Error> {
        todo!()
    }

    fn flush(&mut self) -> anyhow::Result<(), crate::Error> {
        todo!()
    }
}
