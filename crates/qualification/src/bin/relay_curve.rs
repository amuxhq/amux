//! Measures the relay's scaling curve on this machine and prints it.
//! `--baseline` records it as this machine's baseline; `--compare` judges
//! it against that baseline and fails outside the noise bands.

use anyhow::{Context, Result, bail};
use qualification::perf::Machine;
use qualification::perf::relay::{self, Curve, CurveOptions};
use qualification::perf::warmer::{CLUSTER_WARMER_ARG, ClusterWarmer, run_cluster_warmer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Print,
    Baseline,
    Compare,
}

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() == [CLUSTER_WARMER_ARG] {
        return run_cluster_warmer();
    }
    if cfg!(debug_assertions) {
        bail!("the relay curve must run with the release profile");
    }
    let mode = match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Mode::Print,
        ["--baseline"] => Mode::Baseline,
        ["--compare"] => Mode::Compare,
        _ => bail!("the relay curve accepts --baseline or --compare"),
    };
    let machine = Machine::detect()?;
    let path = Curve::baseline_path(&machine);
    let baseline = match mode {
        Mode::Compare => Some(Curve::read(&path)?),
        Mode::Print | Mode::Baseline => None,
    };
    raise_open_files()?;

    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("relay")
        .build()
        .context("start the relay's runtime")?;
    let mut warmer = ClusterWarmer::start()?;
    let curve = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("hosts")
        .build()
        .context("start the hosts' runtime")?
        .block_on(relay::run(
            &machine,
            &CurveOptions::full(),
            relay_runtime.handle(),
        ));
    let stopped = warmer.stop();
    let curve = curve?;
    stopped?;

    curve.print();
    match mode {
        Mode::Print => {}
        Mode::Baseline => {
            curve.write(&path)?;
            println!("baseline: wrote {}", path.display());
        }
        Mode::Compare => {
            let baseline = baseline.expect("read above");
            println!(
                "baseline: {} · load average then {:.2} {:.2} {:.2}",
                path.display(),
                baseline.load_before[0],
                baseline.load_before[1],
                baseline.load_before[2]
            );
            let judgements = relay::compare(&curve, &baseline)?;
            relay::print_judgements(&judgements);
            if judgements.iter().any(|judgement| !judgement.passed) {
                bail!("the relay curve fell outside its noise band against the baseline");
            }
            println!("verdict: PASS, within noise of the baseline");
        }
    }
    Ok(())
}

/// A thousand hosts and the relay's side of each hold two thousand
/// sockets; a shell's default allowance is a few hundred.
#[cfg(unix)]
fn raise_open_files() -> Result<()> {
    const WANTED: libc::rlim_t = 8_192;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid rlimit for the kernel to fill and read.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if limit.rlim_cur < WANTED {
            limit.rlim_cur = WANTED.min(limit.rlim_max);
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    if limit.rlim_cur < WANTED {
        bail!(
            "the relay curve needs {WANTED} open files and this shell allows {}",
            limit.rlim_cur
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn raise_open_files() -> Result<()> {
    Ok(())
}
