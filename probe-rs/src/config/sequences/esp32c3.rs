//! Sequence for the ESP32C3.

use std::sync::Arc;
use std::time::Duration;

use probe_rs_target::Chip;

use crate::{
    architecture::riscv::sequences::RiscvDebugSequence,
    config::sequences::esp::EspFlashSizeDetector, MemoryInterface, Session,
};

/// The debug sequence implementation for the ESP32C3.
#[derive(Debug)]
pub struct ESP32C3 {
    inner: EspFlashSizeDetector,
}

impl ESP32C3 {
    /// Creates a new debug sequence handle for the ESP32C3.
    pub fn create(chip: &Chip) -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: EspFlashSizeDetector::stack_pointer(chip),
                load_address: 0, // Unused for RISC-V
                spiflash_peripheral: 0x6000_2000,
                attach_fn: 0x4000_0164,
            },
        })
    }
}

impl RiscvDebugSequence for ESP32C3 {
    fn on_connect(&self, session: &mut Session) -> Result<(), crate::Error> {
        let interface = session.get_riscv_interface()?;
        tracing::info!("Checking memprot status...");
        if interface.read_word_32(0x600C10A8)? & 0x1 != 0
            || interface.read_word_32(0x600C10C0)? & 0x1 != 0
        {
            // if memprot is enabled, we must reset to disable it
            self.soc_reset(session)?;
        }

        tracing::info!("Disabling esp32c3 watchdogs...");
        let interface = session.get_riscv_interface()?;

        // disable super wdt
        interface.write_word_32(0x600080B0, 0x8F1D312A)?; // write protection off
        let current = interface.read_word_32(0x600080AC)?;
        interface.write_word_32(0x600080AC, current | 1 << 31)?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        interface.write_word_32(0x600080B0, 0x0)?; // write protection on

        // tg0 wdg
        interface.write_word_32(0x6001f064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x6001F048, 0x0)?;
        interface.write_word_32(0x6001f064, 0x0)?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x60020064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x60020048, 0x0)?;
        interface.write_word_32(0x60020064, 0x0)?; // write protection on

        // rtc wdg
        interface.write_word_32(0x600080a8, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x60008090, 0x0)?;
        interface.write_word_32(0x600080a8, 0x0)?; // write protection on

        Ok(())
    }

    fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        self.inner
            .detect_flash_size_riscv(session.get_riscv_interface()?)
    }

    fn soc_reset(&self, session: &mut Session) -> Result<(), crate::Error> {
        tracing::info!("SoC Reset...");
        {
            let mut core = session.core(0)?;
            core.halt(Duration::from_millis(100))?;
            core.reset_catch_set()?;
            core.run()?;
        }

        let interface = session.get_riscv_interface()?;
        // trigger a full SoC reset which resets all domains execept the RTC domain
        interface.write_word_32(0x60008000, 0x9c00a000)?;
        interface.write_word_32(0x6001F068, 0)?;
        // Workaround for stuck in cpu start during calibration.
        // By writing zero to TIMG_RTCCALICFG_REG, we are disabling calibration
        interface.write_word_32(0x6001F068, 0)?;

        {
            let mut core = session.core(0)?;
            // wait for the reset to happen
            core.wait_for_core_halted(std::time::Duration::from_millis(100))?;
            tracing::info!("Caught reset");
            core.reset_catch_clear()?;
        }

        Ok(())
    }
}
