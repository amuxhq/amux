//! The `claude` and `codex` commands a host's agents run.
//!
//! Every way an agent comes to exist ends with its host launching its
//! configured provider command: a spawn from the net, a create from the
//! terminal client or the phone, an agent's own spawn tool, a resume. Only
//! the first carries arguments the net chose, and clients must not be
//! changed for a test, so the net decides at the one point they share: the
//! command. Each host gets a `claude` and a `codex` script that find which
//! agent is starting them (the `amux agent <dir>` above them), read that
//! agent's choice if the net recorded one, and run either the scripted
//! fake or the real provider.
//!
//! The real provider runs through its recording probe, so what amux wrote
//! to it and read back is kept, and with the operator's own setup kept out:
//! Claude reads only project settings and only the tool server amux names,
//! with self-updates off, under the operator's own login (a scratch config
//! directory would lose it); Codex runs on a scratch home holding only the
//! operator's login.

use std::path::{Path, PathBuf};

use provider_fakes::script::{INPUT_LOG_ENV, SCRIPT_ENV};

use crate::topology::{FakeKind, Provider};

/// The real providers and their recording probes.
#[derive(Clone, Debug)]
pub(crate) struct RealTools {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub claude_probe: PathBuf,
    pub codex_probe: PathBuf,
}

impl RealTools {
    /// The first `claude` and `codex` on this process's PATH, as absolute
    /// paths, and the probes beside the net's binaries.
    pub(crate) fn find(binaries: &Path) -> Result<Self, String> {
        let on_path = |command: &str| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            std::env::split_paths(&path)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .and_then(|found| std::path::absolute(found).ok())
                .ok_or_else(|| format!("no {command} on the PATH for a real provider"))
        };
        let probe = |name: &str| {
            let path = binaries.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            if path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "{} is not built; a real provider records through it \
                     (cargo build -p claude-specs -p codex-specs --bins, or just qa)",
                    path.display()
                ))
            }
        };
        Ok(Self {
            claude: on_path("claude")?,
            codex: on_path("codex")?,
            claude_probe: probe("claude-probe")?,
            codex_probe: probe("codex-probe")?,
        })
    }
}

/// Where a host's provider commands come from and what they default to.
pub(crate) struct Wrappers<'a> {
    pub host: &'a str,
    /// The host's own directory: the scripts go in `bin/` below it.
    pub dir: &'a Path,
    pub default: Provider,
    /// Where each agent's choice is recorded, as `<agent id>` files.
    pub choices: &'a Path,
    /// Where real agents' traffic is recorded, a folder per agent.
    pub recordings: &'a Path,
    pub real: Option<&'a RealTools>,
    /// The fakes' binaries.
    pub fakes: &'a Path,
    /// What a fake plays for an agent the net recorded no choice for.
    pub host_script: Option<&'a Path>,
}

/// The two commands, in the host's `bin/`.
pub(crate) struct Commands {
    pub claude: PathBuf,
    pub codex: PathBuf,
}

/// Records an agent's provider for the host's commands to find when it
/// starts: the real one, or a fake of `kind` playing `script`.
pub(crate) fn choose(
    choices: &Path,
    agent: uuid::Uuid,
    provider: Provider,
    kind: FakeKind,
    script: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(choices)?;
    let provider = match provider {
        Provider::Real => "real",
        Provider::Fake => "fake",
    };
    let kind = match kind {
        FakeKind::ClaudePty => "claude_pty",
        FakeKind::ClaudeSdk => "claude_sdk",
        FakeKind::Codex => "codex",
    };
    std::fs::write(
        choices.join(agent.to_string()),
        format!(
            "provider={provider}\nkind={kind}\nscript={}\ninput={}\n",
            quote(script),
            quote(&script.with_extension("input")),
        ),
    )
}

/// The scratch Codex home a real host's agents run on: only the operator's
/// login, so their configuration, tool servers and history stay out.
pub(crate) fn seed_codex_home(dir: &Path) -> Result<PathBuf, String> {
    let source = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or("no Codex login: neither CODEX_HOME nor HOME is set")?;
    let auth = source.join("auth.json");
    if !auth.exists() {
        return Err(format!(
            "no Codex login: {} does not exist; run `codex login`",
            auth.display()
        ));
    }
    let home = dir.join("codex-home");
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    std::fs::copy(&auth, home.join("auth.json")).map_err(|error| error.to_string())?;
    // Codex refuses a pristine home before it starts its app server; the
    // markers are its local setup, nothing of the operator's. The
    // operator's plugins and connected apps come with the account, not the
    // home, so the config switches them off: the agent then carries only
    // amux's tool server and Codex's own built-in skills.
    for (file, text) in [
        (
            "config.toml",
            "[features]\nplugins = false\nremote_plugin = false\napps = false\n".to_owned(),
        ),
        (".personality_migration", "v1\n".to_owned()),
        (".sandbox_migration", "v1\n".to_owned()),
        ("installation_id", format!("{}\n", uuid::Uuid::new_v4())),
    ] {
        std::fs::write(home.join(file), text).map_err(|error| error.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(home)
}

/// Finds the agent this command runs for: the `amux agent <dir>` process
/// above it, a few generations up at most (a terminal provider runs under
/// its pseudo-terminal's helper). None for a command the daemon runs
/// itself, such as a version check or a catalogue refresh.
const FIND_AGENT: &str = r#"agent=
pid=$PPID
hops=0
while [ "$hops" -lt 6 ] && [ -n "$pid" ] && [ "$pid" -gt 1 ]; do
    args=$(ps -o args= -p "$pid" 2>/dev/null) || break
    case "$args" in
        *" agent "*/agents/*)
            agent=${args##*/agents/}
            agent=${agent%%[ /]*}
            break
            ;;
    esac
    pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
    hops=$((hops + 1))
done
"#;

pub(crate) fn write(wrappers: &Wrappers<'_>) -> Result<Commands, String> {
    let bin = wrappers.dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(|error| error.to_string())?;
    let codex_home = match wrappers.real {
        Some(_) => Some(seed_codex_home(wrappers.dir)?),
        None => None,
    };
    let fake = |kind: FakeKind| quote(&wrappers.fakes.join(kind.binary()));
    let header = |command: &str| {
        format!(
            "#!/bin/sh\n# {command} for testnet host {host}: the scripted fake or the real {command},\n\
             # by the choice recorded for the agent starting it.\n{FIND_AGENT}\
             provider={default}\nkind=\nscript={script}\ninput={input}\n\
             if [ -n \"$agent\" ] && [ -f {choices}/\"$agent\" ]; then . {choices}/\"$agent\"; fi\n",
            host = wrappers.host,
            default = match wrappers.default {
                Provider::Real => "real",
                Provider::Fake => "fake",
            },
            script = wrappers.host_script.map(quote).unwrap_or_default(),
            input = wrappers
                .host_script
                .map(|script| quote(&script.with_extension("input")))
                .unwrap_or_default(),
            choices = quote(wrappers.choices),
        )
    };
    let real_branch = |prefix: &str, real: Option<(&Path, &Path)>, setup: &str| match real {
        Some((probe, real)) => format!(
            "if [ \"$provider\" = real ]; then\n\
             \x20   dir={recordings}/\"${{agent:-host}}\"\n\
             \x20   mkdir -p \"$dir\"\n\
             \x20   # The real provider must not find this script again on the PATH.\n\
             \x20   PATH=$(printf '%s' \"$PATH\" | tr ':' '\\n' | grep -vxF {bin} | paste -sd: -)\n\
             \x20   export PATH\n\
             \x20   unset {script_env} {input_env}\n\
             \x20   export {prefix}_CAPTURE_PROXY=1 {prefix}_CAPTURE_DIR=\"$dir\" {prefix}_REAL_PATH={real}\n\
             {setup}\
             fi\n",
            recordings = quote(wrappers.recordings),
            bin = quote(&bin),
            script_env = SCRIPT_ENV,
            input_env = INPUT_LOG_ENV,
            real = quote(real),
            setup = setup.replace("{probe}", &quote(probe)),
        ),
        None => "if [ \"$provider\" = real ]; then\n\
                 \x20   echo \"testnet: this net has no real providers set up\" >&2\n\
                 \x20   exit 70\n\
                 fi\n"
            .to_owned(),
    };
    let fake_tail = |fakes: &str| {
        format!(
            "{fakes}\
             if [ -n \"$script\" ]; then export {SCRIPT_ENV}=\"$script\" {INPUT_LOG_ENV}=\"$input\"; fi\n\
             exec \"$fake\" \"$@\"\n"
        )
    };
    let claude = format!(
        "{}{}{}",
        header("claude"),
        real_branch(
            "CLAUDE",
            wrappers
                .real
                .map(|real| (real.claude_probe.as_path(), real.claude.as_path())),
            "    unset CLAUDE_CONFIG_DIR\n\
             \x20   export DISABLE_AUTOUPDATER=1 DISABLE_UPDATES=1 DISABLE_INSTALLATION_CHECKS=1\n\
             \x20   exec {probe} --setting-sources project --strict-mcp-config \"$@\"\n",
        ),
        fake_tail(&format!(
            "case \"$kind\" in claude_pty) fake={} ;; *) fake={} ;; esac\n\
             export CLAUDE_CONFIG_DIR={}\n",
            fake(FakeKind::ClaudePty),
            fake(FakeKind::ClaudeSdk),
            quote(&wrappers.dir.join("claude")),
        )),
    );
    let codex_setup = codex_home
        .as_deref()
        .map(|home| {
            // Codex also reads skills from the user's home, outside its
            // own: a home of its own keeps the operator's out.
            let user = home.with_file_name("codex-user-home");
            format!(
                "    export CODEX_HOME={}\n    mkdir -p {user}\n    export HOME={user}\n    exec {{probe}} \"$@\"\n",
                quote(home),
                user = quote(&user),
            )
        })
        .unwrap_or_default();
    let codex = format!(
        "{}{}{}",
        header("codex"),
        real_branch(
            "CODEX",
            wrappers
                .real
                .map(|real| (real.codex_probe.as_path(), real.codex.as_path())),
            &codex_setup,
        ),
        fake_tail(&format!("fake={}\n", fake(FakeKind::Codex))),
    );
    let commands = Commands {
        claude: bin.join("claude"),
        codex: bin.join("codex"),
    };
    for (path, text) in [(&commands.claude, claude), (&commands.codex, codex)] {
        std::fs::write(path, text).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(commands)
}

/// A path as one shell word.
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}
