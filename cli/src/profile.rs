use addr2line::{
    gimli::{EndianRcSlice, RunTimeEndian},
    object::{read::File as ObjectFile, Object},
    Context as ObjectContext, LookupResult,
};
use anyhow::Context;
use probe_rs::{flashing::FileDownloadError, debug::DebugInfo};
use probe_rs_cli_util::{
    clap,
    common_options::{CargoOptions, FlashOptions, ProbeOptions},
    flash::run_flash_download,
};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::time::Duration;
use strum::{Display, EnumIter, EnumString, EnumVariantNames};
use time::Instant;
use tracing::info;

#[derive(clap::Parser)]
pub struct ProfileOptions {
    /// Flash the ELF before profiling
    #[clap(long)]
    flash: bool,
    /// Do a full chip erase before flashing
    #[clap(long)]
    chip_erase: bool,
    /// Print file and line info for each entry
    #[clap(long)]
    line_info: bool,
    /// Disable double-buffering when downloading flash.  If downloading times out, try this option.
    #[clap(long = "disable-double-buffering")]
    disable_double_buffering: bool,
    // Duration of profile in seconds.
    #[clap(long)]
    duration: u64, // Option<u64> If we could catch ctrl-c we can make this optional
    /// Which core to profile
    #[clap(long, default_value_t = 0)]
    core: usize,
    /// Limit the number of entries to output
    #[clap(long, default_value_t = 25)]
    limit: usize,
    #[clap(long, default_value_t = ProfileMethod::Naive)]
    /// Profile Method
    method: ProfileMethod,
}

#[derive(
    clap::ValueEnum,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    EnumIter,
    EnumString,
    EnumVariantNames,
)]
#[strum(serialize_all = "lowercase")]
#[non_exhaustive]
pub enum ProfileMethod {
    /// Naive, Halt -> Read PC -> Resume profiler
    Naive,
}

pub fn profile(
    common: ProbeOptions,
    path: &str,
    profile_opts: ProfileOptions,
) -> anyhow::Result<()> {
    let mut session = common.simple_attach()?;
    info!("Attached");

    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
    };

    let symbols = DebugInfo::from_file(path)?;

    if profile_opts.flash {
        let mut loader = session.target().flash_loader();
        loader.load_elf_data(&mut file)?;

        run_flash_download(
            &mut session,
            Path::new(path),
            &FlashOptions {
                list_chips: false,
                list_probes: false,
                disable_progressbars: false,
                disable_double_buffering: profile_opts.disable_double_buffering,
                reset_halt: false,
                log: None,
                restore_unwritten: false,
                flash_layout_output_path: None,
                elf: None,
                work_dir: None,
                cargo_options: CargoOptions::default(),
                probe_options: common,
            },
            loader,
            profile_opts.chip_erase,
        )?;
    }

    let mut core = session.core(profile_opts.core)?;
    info!("Attached to Core {}", profile_opts.core);
    core.reset()?;

    let start = Instant::now();
    let mut reads = 0;
    let mut samples: HashMap<u32, u64> =
        HashMap::with_capacity(256 * (profile_opts.duration as usize));
    let duration = Duration::from_secs(profile_opts.duration);
    let pc_reg = core.program_counter();
    info!("Profiling...");
    loop {
        core.halt(std::time::Duration::from_millis(10))?;
        let pc: u32 = core.read_core_reg(pc_reg)?;
        *samples.entry(pc).or_insert(1) += 1;
        reads += 1;
        core.run()?;
        if Instant::now() - start > duration {
            break;
        }
    }

    let mut v = Vec::from_iter(samples);
    // sort by frequency
    v.sort_by(|&(_, a), &(_, b)| b.cmp(&a));

    for (address, count) in v.into_iter().take(profile_opts.limit) {
        let name = symbols
            .function_name(address as u64, true)?
            .unwrap_or(format!("UNKNOWN - {:08X}", address));
        let source = symbols
            .get_source_location(address as u64);
        if profile_opts.line_info {
            if let Some(source) = source {
                println!("{}:{}", source.file.unwrap_or("UNKNOWN".to_owned()), source.line.unwrap_or(0));
            } else {
                println!("UNKNOWN:0");
            }
        }
        println!(
            "{:>50} - {:.01}%",
            name,
            (count as f64 / reads as f64) * 100.0
        );
    }

    Ok(())
}
