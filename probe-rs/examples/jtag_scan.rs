use anyhow::Result;
use probe_rs::Probe;

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let mut probe: Probe = probes[0].open()?;

    probe.set_speed(100)?;
    probe.attach_to_unspecified()?;

    Ok(())
}
