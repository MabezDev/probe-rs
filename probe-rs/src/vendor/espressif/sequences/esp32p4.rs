//! Sequences for the ESP32P4.

use std::{sync::Arc, time::Duration};

use crate::{
    architecture::riscv::{
        communication_interface::{RiscvCommunicationInterface, Sbaddress0, Sbcs, Sbdata0},
        sequences::RiscvDebugSequence,
        Dmcontrol
    },
    MemoryInterface, Session,
};

/// The debug sequence implementation for the ESP32P4.
#[derive(Debug)]
pub struct ESP32P4;

impl ESP32P4 {
    /// Creates a new debug sequence handle for the ESP32P4.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }
}

impl RiscvDebugSequence for ESP32P4 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32P4 watchdogs...");
        // tg0 wdg
        interface.write_word_32(0x500c2064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x500c2048, 0x0)?;
        interface.write_word_32(0x500c2064, 0x0)?; // write protection on
        interface.write_word_32(0x500c207c, 0x4)?; // clear interrupt state

        // tg1 wdg
        interface.write_word_32(0x500c3064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x500c3048, 0x0)?;
        interface.write_word_32(0x500c3064, 0x0)?; // write protection on
        interface.write_word_32(0x500c307c, 0x4)?; // clear interrupt state

        // rtc wdg
        interface.write_word_32(0x50116018, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x50116000, 0x0)?;
        interface.write_word_32(0x50116030, 0x0)?; // write protection on
        interface.write_word_32(0x50116030, 0xC0000000)?; // clear interrupt state

        Ok(())
    }

    fn detect_flash_size(&self, _interface: &mut Session) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms.
    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        tracing::info!("executing reset_system_and_halt");
        interface.halt(timeout)?;

        // System reset, ported from OpenOCD.
        interface.write_dm_register(Sbcs(0x40000))?;
        // Writing LP_SYS_SYS_CTRL_REG causes the System Reset
	    // System Reset: resets the whole digital system, including the LP system.
        interface.write_dm_register(Sbaddress0(0x50110008))?;
        // Set (LP_SYS_SYS_SW_RST|LP_SYS_DIG_FIB|LP_SYS_ANA_FIB|LP_SYS_LP_FIB_SEL)
        interface.write_dm_register(Sbdata0(0x1fffc7fa))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0))?;

        interface.write_dm_register(Sbcs(0x40000))?;
        interface.write_dm_register(Sbaddress0(0x600b1038))?;
        interface.write_dm_register(Sbdata0(0x10000000_u32))?;

        // I don't think this is needed on P4
        // // clear dmactive to clear sbbusy otherwise debug module gets stuck
        // interface.write_dm_register(Dmcontrol(0))?;

        // let mut dmcontrol = Dmcontrol(0);
        // dmcontrol.set_dmactive(true);
        // dmcontrol.set_resumereq(true);
        // interface.write_dm_register(dmcontrol)?;

        // wait for the reset to happen
        std::thread::sleep(Duration::from_millis(10));

        // I think P4 rev1 isn't enabling usb jtag in ROM code when we reset, meaning this currently doesn't work
        // Need to get a eco2 board at least to try it

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol)?;

        interface.enter_debug_mode()?;
        self.on_connect(interface)?;

        interface.reset_hart_and_halt(timeout)?;

        Ok(())
    }
}
