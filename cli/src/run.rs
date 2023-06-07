use anyhow::Result;
use probe_rs::flashing::Format;
use probe_rs_cli_util::common_options::ProbeOptions;
use probe_rs_cli_util::rtt;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use time::UtcOffset;

use crate::download_program_fast;

pub fn run(
    common: ProbeOptions,
    path: &str,
    chip_erase: bool,
    disable_double_buffering: bool,
    timestamp_offset: UtcOffset,
    format: Format,
) -> Result<()> {
    let mut session = download_program_fast(common, format, path, chip_erase, false, disable_double_buffering)?;

    let rtt_config = rtt::RttConfig::default();

    let memory_map = session.target().memory_map.clone();

    let mut core = session.core(0)?;
    core.reset()?;

    let mut rtta = match rtt::attach_to_rtt(
        &mut core,
        &memory_map,
        Path::new(path),
        &rtt_config,
        timestamp_offset,
    ) {
        Ok(target_rtt) => Some(target_rtt),
        Err(error) => {
            log::error!("{:?} Continuing without RTT... ", error);
            None
        }
    };

    if let Some(rtta) = &mut rtta {
        let mut stdout = std::io::stdout();
        loop {
            for (_ch, data) in rtta.poll_rtt_fallible(&mut core)? {
                stdout.write_all(data.as_bytes())?;
            }

            // Poll RTT with a frequency of 10 Hz
            //
            // If the polling frequency is too high,
            // the USB connection to the probe can become unstable.
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    Ok(())
}
