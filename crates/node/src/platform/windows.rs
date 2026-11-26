//! Windows.

use std::ffi::OsStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawSocket as _;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, ptr};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BUFFER_OVERFLOW, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, HANDLE,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IF_TYPE_TUNNEL, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, IpDadStatePreferred, IpSuffixOriginRandom, SIO_UDP_CONNRESET,
    SO_EXCLUSIVEADDRUSE, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKET, SOCKET_ERROR, SOL_SOCKET,
    WSAGetLastError, WSAIoctl, setsockopt,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RRF_RT_REG_DWORD, RegCloseKey, RegEnumValueW,
    RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::firewall::{Change, Profiles, RULE_NAME, Rule, Snapshot};

/// Windows names no boot id; the boot time, from the wall clock minus
/// the uptime, is one. It is rounded to ten seconds because the two
/// readings drift apart by milliseconds; a reading that lands across a
/// rounding edge looks like a reboot, which bumps the generation only
/// after an unclean shutdown, the safe direction.
pub fn boot_id() -> io::Result<String> {
    // SAFETY: no arguments; reads the system's uptime counter.
    let uptime_ms = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis() as u64;
    Ok(format!(
        "boot-{}",
        now_ms.saturating_sub(uptime_ms) / 10_000
    ))
}

// The link's UDP socket ----------------------------------------------------

/// SO_EXCLUSIVEADDRUSE: without it a socket that sets SO_REUSEADDR binds
/// the same port and takes datagrams meant for amux.
pub fn claim_port(socket: &socket2::Socket) -> io::Result<()> {
    let on: i32 = 1;
    // SAFETY: the socket is open; `on` outlives the call, which reads its
    // four bytes.
    let status = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&raw const on).cast(),
            size_of::<i32>() as i32,
        )
    };
    if status == SOCKET_ERROR {
        return Err(last_socket_error());
    }
    Ok(())
}

/// SIO_UDP_CONNRESET off: Windows otherwise reports the ICMP port
/// unreachable an earlier send brought back as WSAECONNRESET on the next
/// receive, which says nothing about the datagram being received.
pub fn ignore_unreachable(socket: &std::net::UdpSocket) -> io::Result<()> {
    let off: u32 = 0;
    let mut returned = 0u32;
    // SAFETY: the socket is open; the input is `off`'s four bytes, there
    // is no output, and the call completes before it returns (no
    // OVERLAPPED).
    let status = unsafe {
        WSAIoctl(
            socket.as_raw_socket() as SOCKET,
            SIO_UDP_CONNRESET,
            (&raw const off).cast(),
            size_of::<u32>() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
            None,
        )
    };
    if status == SOCKET_ERROR {
        return Err(last_socket_error());
    }
    Ok(())
}

fn last_socket_error() -> io::Error {
    // SAFETY: reads this thread's last Winsock error.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

// Interfaces ---------------------------------------------------------------

/// One adapter, as much as decides whether its addresses are listed.
#[derive(Clone, Debug)]
struct Adapter {
    if_type: u32,
    up: bool,
    description: String,
    has_gateway: bool,
    addresses: Vec<Address>,
}

#[derive(Clone, Copy, Debug)]
struct Address {
    ip: IpAddr,
    /// Neither still being checked for duplicates nor deprecated.
    preferred: bool,
    /// An IPv6 temporary (privacy) address, which Windows replaces every
    /// day or so.
    temporary: bool,
}

/// The adapters GetAdaptersAddresses reports, filtered: only adapters that
/// are up; no tunnel (Teredo, 6to4, IP-HTTPS), whose addresses no device
/// nearby reaches; no Hyper-V virtual switch of the host's own without a
/// gateway (WSL's and the Default Switch, whose networks are this
/// machine's VMs); no 169.254 address, which an adapter takes when no
/// network answered it; and no IPv6 temporary address beside a stable
/// one, so its daily replacement doesn't count as a network change.
pub fn interface_ips() -> io::Result<Vec<IpAddr>> {
    Ok(listed(&adapters()?))
}

fn listed(adapters: &[Adapter]) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for adapter in adapters {
        if !adapter.up
            || adapter.if_type == IF_TYPE_TUNNEL
            || (adapter
                .description
                .starts_with("Hyper-V Virtual Ethernet Adapter")
                && !adapter.has_gateway)
        {
            continue;
        }
        let stable_v6 = adapter
            .addresses
            .iter()
            .any(|address| address.preferred && !address.temporary && global_v6(address.ip));
        ips.extend(
            adapter
                .addresses
                .iter()
                .filter(|address| address.preferred)
                .filter(|address| !(address.temporary && stable_v6))
                .filter(|address| !matches!(address.ip, IpAddr::V4(ip) if ip.is_link_local()))
                .map(|address| address.ip),
        );
    }
    ips
}

fn global_v6(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(ip) if !ip.is_loopback() && ip.segments()[0] & 0xffc0 != 0xfe80)
}

fn adapters() -> io::Result<Vec<Adapter>> {
    let flags = GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER
        | GAA_FLAG_INCLUDE_GATEWAYS;
    let mut size = 16 * 1024u32;
    // u64 words keep the IP_ADAPTER_ADDRESSES_LH the buffer starts with
    // aligned.
    let mut buffer: Vec<u64>;
    loop {
        buffer = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: `buffer` is `size` bytes, writable and aligned.
        let status = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                flags,
                ptr::null(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        match status {
            ERROR_SUCCESS => break,
            ERROR_BUFFER_OVERFLOW => continue,
            // No adapter has an address.
            ERROR_NO_DATA_ADAPTERS => return Ok(Vec::new()),
            error => return Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
    let mut adapters = Vec::new();
    let mut next = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !next.is_null() {
        // SAFETY: each entry, and what it points at, lies in `buffer`,
        // which lives until this function returns.
        let adapter = unsafe { &*next };
        let mut addresses = Vec::new();
        let mut unicast = adapter.FirstUnicastAddress;
        while !unicast.is_null() {
            // SAFETY: as above.
            let address = unsafe { &*unicast };
            // SAFETY: as above; a null or foreign address is skipped.
            if let Some(ip) = unsafe { ip_of(address.Address.lpSockaddr) } {
                addresses.push(Address {
                    ip,
                    preferred: address.DadState == IpDadStatePreferred,
                    temporary: address.SuffixOrigin == IpSuffixOriginRandom,
                });
            }
            unicast = address.Next;
        }
        adapters.push(Adapter {
            if_type: adapter.IfType,
            up: adapter.OperStatus == IfOperStatusUp,
            // SAFETY: a NUL-terminated string in `buffer`, or null.
            description: unsafe { wide_text(adapter.Description) },
            has_gateway: !adapter.FirstGatewayAddress.is_null(),
            addresses,
        });
        next = adapter.Next;
    }
    Ok(adapters)
}

/// The value GetAdaptersAddresses returns when no adapter has an address.
const ERROR_NO_DATA_ADAPTERS: u32 = windows_sys::Win32::Foundation::ERROR_NO_DATA;

/// # Safety
/// `addr` is null or points at a socket address of its family's size.
unsafe fn ip_of(addr: *const SOCKADDR) -> Option<IpAddr> {
    if addr.is_null() {
        return None;
    }
    // SAFETY: the caller's.
    match unsafe { (*addr).sa_family } {
        AF_INET => {
            // SAFETY: an AF_INET address is a SOCKADDR_IN.
            let v4 = unsafe { &*addr.cast::<SOCKADDR_IN>() };
            // SAFETY: every field of the union is the same four bytes.
            let raw = unsafe { v4.sin_addr.S_un.S_addr };
            Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(raw))))
        }
        AF_INET6 => {
            // SAFETY: an AF_INET6 address is a SOCKADDR_IN6.
            let v6 = unsafe { &*addr.cast::<SOCKADDR_IN6>() };
            // SAFETY: as above, sixteen bytes.
            Some(IpAddr::V6(Ipv6Addr::from(unsafe { v6.sin6_addr.u.Byte })))
        }
        _ => None,
    }
}

/// # Safety
/// `text` is null or a NUL-terminated UTF-16 string.
unsafe fn wide_text(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller's: the string ends at a NUL.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units, all read above.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
}

// The firewall -------------------------------------------------------------

/// Windows Defender Firewall: its rules and profiles read from the
/// registry, where it keeps them (any user may read them), and changed
/// with `netsh advfirewall` and PowerShell's `Remove-NetFirewallRule`,
/// which need administrator rights.
pub struct Firewall;

const FIREWALL_POLICY: &str =
    r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy";
const GROUP_POLICY: &str = r"SOFTWARE\Policies\Microsoft\WindowsFirewall";

impl super::firewall::System for Firewall {
    fn read(&self) -> io::Result<Snapshot> {
        let mut rules = Vec::new();
        for store in [FIREWALL_POLICY, GROUP_POLICY] {
            let values = string_values(&format!(r"{store}\FirewallRules"))?;
            rules.extend(values.into_iter().filter_map(|(id, text)| {
                Some(Rule {
                    id,
                    policy: store == GROUP_POLICY,
                    ..Rule::parse(&text)?
                })
            }));
        }
        // A group policy's setting wins over the machine's own; with
        // neither, the firewall is on, as Windows starts it.
        let on = |local: &str, policy: &str| -> io::Result<bool> {
            let named = |store: &str, profile: &str| {
                dword(&format!(r"{store}\{profile}"), "EnableFirewall")
            };
            Ok(match named(GROUP_POLICY, policy)? {
                Some(value) => value != 0,
                None => named(FIREWALL_POLICY, local)?.is_none_or(|value| value != 0),
            })
        };
        // The machine's own rules count beside a group policy's unless the
        // policy says otherwise for that profile.
        let merged = |policy: &str| -> io::Result<bool> {
            Ok(dword(
                &format!(r"{GROUP_POLICY}\{policy}"),
                "AllowLocalPolicyMerge",
            )?
            .is_none_or(|value| value != 0))
        };
        Ok(Snapshot {
            rules,
            on: Profiles {
                domain: on("DomainProfile", "DomainProfile")?,
                private: on("StandardProfile", "PrivateProfile")?,
                public: on("PublicProfile", "PublicProfile")?,
            },
            local_rules: Profiles {
                domain: merged("DomainProfile")?,
                private: merged("PrivateProfile")?,
                public: merged("PublicProfile")?,
            },
        })
    }

    fn elevated(&self) -> io::Result<bool> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: the pseudo-handle of this process; `token` receives a
        // handle closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        // SAFETY: `elevation` is the size passed and outlives the call.
        let read = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                (&raw mut elevation).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: the handle OpenProcessToken gave, closed once.
        unsafe { CloseHandle(token) };
        if read == 0 {
            return Err(error);
        }
        Ok(elevation.TokenIsElevated != 0)
    }

    fn apply(&self, changes: &[Change]) -> io::Result<()> {
        for change in changes {
            let (program, arguments, doing) = match change {
                Change::Remove { id, name } => (
                    powershell()?,
                    powershell_arguments(id),
                    format!("remove the rule {name:?}"),
                ),
                Change::Allow { program } => (
                    netsh()?,
                    netsh_arguments(program),
                    "add the rule".to_owned(),
                ),
            };
            let output = std::process::Command::new(&program)
                .raw_arg(arguments)
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "{} could not {doing}: {}{}",
                    program.display(),
                    String::from_utf8_lossy(&output.stdout).trim(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        Ok(())
    }

    fn run_as_administrator(&self, program: &Path, args: &[&str]) -> io::Result<u32> {
        let verb = wide("runas");
        let file = wide(program.as_os_str());
        let parameters = wide(args.join(" "));
        // SAFETY: an all-zero SHELLEXECUTEINFOW is a valid empty request.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        // The run's console stays hidden; it has nothing to ask.
        info.nShow = SW_HIDE;
        // SAFETY: `info` and the strings it points at outlive the call.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the request for administrator approval was declined",
                ));
            }
            return Err(error);
        }
        let process = info.hProcess;
        if process.is_null() {
            return Err(io::Error::other(
                "the run as an administrator gave no process",
            ));
        }
        let mut code = 0u32;
        // SAFETY: the process handle SEE_MASK_NOCLOSEPROCESS gave us,
        // waited on, read and closed once.
        let read = unsafe {
            WaitForSingleObject(process, INFINITE);
            let read = GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
            read
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code)
    }
}

/// netsh.exe in the system folder, never one found on PATH: it runs with
/// administrator rights.
fn netsh() -> io::Result<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot is not set"))?;
    Ok(Path::new(&root).join("System32").join("netsh.exe"))
}

/// Windows PowerShell in the system folder, never one found on PATH: it
/// runs with administrator rights.
fn powershell() -> io::Result<std::path::PathBuf> {
    Ok(netsh()?
        .with_file_name("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe"))
}

/// The command line after `powershell` that removes the machine's own rule
/// kept under `id`, and fails unless exactly that one rule was there.
/// netsh names rules by their name, which many share (Windows' prompt
/// names each it adds after the program), so the rule is found by its id,
/// compared whole rather than as `-Name`'s pattern, in the store of the
/// machine's own rules. The script goes encoded, so no quoting of the id
/// is left to a command line.
fn powershell_arguments(id: &str) -> String {
    use base64::Engine as _;
    let script = format!(
        "$ErrorActionPreference = 'Stop'; $id = '{}'; \
         $rules = @(Get-NetFirewallRule -PolicyStore PersistentStore | Where-Object {{ $_.Name -eq $id }}); \
         if ($rules.Count -ne 1) {{ [Console]::Error.WriteLine(\"$($rules.Count) rules have the id $id\"); exit 2 }}; \
         $rules | Remove-NetFirewallRule",
        id.replace('\'', "''")
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "-NoProfile -NonInteractive -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(utf16)
    )
}

/// The command line after `netsh` that adds [`RULE_NAME`] for `program`,
/// as netsh parses it: the path in quotes, which no Windows path contains.
fn netsh_arguments(program: &Path) -> String {
    format!(
        r#"advfirewall firewall add rule name="{RULE_NAME}" dir=in action=allow program="{}" protocol=udp profile=private,domain enable=yes description="Lets devices nearby reach amux: links, pairing and discovery on the local network.""#,
        program.display()
    )
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}

/// A key under HKEY_LOCAL_MACHINE opened to read, closed on drop; none
/// when it doesn't exist.
struct Key(HKEY);

impl Key {
    fn open(subkey: &str) -> io::Result<Option<Key>> {
        let name = wide(subkey);
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: `name` is NUL-terminated; `key` receives a handle the
        // returned Key closes.
        let status =
            unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, name.as_ptr(), 0, KEY_READ, &mut key) };
        match status {
            ERROR_SUCCESS => Ok(Some(Key(key))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            error => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: a key RegOpenKeyExW opened, closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

/// Every string value of a key under HKEY_LOCAL_MACHINE, by name; none
/// when the key doesn't exist.
fn string_values(subkey: &str) -> io::Result<Vec<(String, String)>> {
    let Some(key) = Key::open(subkey)? else {
        return Ok(Vec::new());
    };
    let (mut count, mut longest_name, mut longest_data) = (0u32, 0u32, 0u32);
    // SAFETY: the key is open; every pointer is null or a u32 that
    // outlives the call.
    let status = unsafe {
        RegQueryInfoKeyW(
            key.0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut longest_name,
            &mut longest_data,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut values = Vec::with_capacity(count as usize);
    let mut name = vec![0u16; longest_name as usize + 1];
    let mut data = vec![0u16; (longest_data as usize).div_ceil(2) + 1];
    for index in 0.. {
        let mut name_len = name.len() as u32;
        let mut data_len = (data.len() * 2) as u32;
        let mut kind = 0u32;
        // SAFETY: the buffers are the sizes passed and outlive the call.
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                ptr::null(),
                &mut kind,
                data.as_mut_ptr().cast(),
                &mut data_len,
            )
        };
        match status {
            ERROR_SUCCESS if kind == REG_SZ => {
                let text = &data[..(data_len as usize / 2).min(data.len())];
                let text = text.split(|unit| *unit == 0).next().unwrap_or_default();
                let named = &name[..(name_len as usize).min(name.len())];
                values.push((
                    String::from_utf16_lossy(named),
                    String::from_utf16_lossy(text),
                ));
            }
            ERROR_NO_MORE_ITEMS => break,
            // Not a string, or one written since the key was measured,
            // longer than any then: not a rule this read can use.
            _ => {}
        }
    }
    Ok(values)
}

/// A DWORD value of a key under HKEY_LOCAL_MACHINE; none when the key or
/// the value doesn't exist.
fn dword(subkey: &str, value: &str) -> io::Result<Option<u32>> {
    let (subkey, value) = (wide(subkey), wide(value));
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the names are NUL-terminated; `data` is `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    match status {
        ERROR_SUCCESS => Ok(Some(data)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::platform::firewall::{Standing, System as _, standing};

    fn adapter(description: &str, ips: &[&str]) -> Adapter {
        Adapter {
            if_type: 6,
            up: true,
            description: description.to_owned(),
            has_gateway: true,
            addresses: ips
                .iter()
                .map(|ip| Address {
                    ip: ip.parse().unwrap(),
                    preferred: true,
                    temporary: false,
                })
                .collect(),
        }
    }

    fn ips(listed: Vec<IpAddr>) -> Vec<String> {
        listed.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_listed_addresses_are_the_ones_devices_nearby_can_reach() {
        let ethernet = adapter(
            "Intel(R) Ethernet Connection",
            &["192.168.1.20", "fd00::20"],
        );
        let mut down = adapter("Realtek Wi-Fi", &["192.168.7.3"]);
        down.up = false;
        let mut teredo = adapter("Teredo Tunneling Pseudo-Interface", &["2001:0:1234::1"]);
        teredo.if_type = IF_TYPE_TUNNEL;
        let mut wsl = adapter("Hyper-V Virtual Ethernet Adapter", &["172.29.16.1"]);
        wsl.has_gateway = false;
        let mut default_switch = adapter("Hyper-V Virtual Ethernet Adapter #2", &["172.20.0.1"]);
        default_switch.has_gateway = false;
        let apipa = adapter("USB Ethernet", &["169.254.10.2"]);
        // A Hyper-V switch bound to the LAN is how the host itself reaches
        // it: its address stays.
        let external = adapter("Hyper-V Virtual Ethernet Adapter #3", &["10.0.0.5"]);
        // Tailscale is no tunnel to Windows (IfType 53): it stays.
        let mut tailscale = adapter("Tailscale Tunnel", &["100.96.110.125"]);
        tailscale.if_type = 53;
        assert_eq!(
            ips(listed(&[
                ethernet,
                down,
                teredo,
                wsl,
                default_switch,
                apipa,
                external,
                tailscale
            ])),
            ["192.168.1.20", "fd00::20", "10.0.0.5", "100.96.110.125"]
        );
    }

    #[test]
    fn a_temporary_ipv6_address_is_left_out_beside_a_stable_one() {
        let mut wifi = adapter("Wi-Fi", &["fdb2::1", "fdb2::2", "fe80::3"]);
        wifi.addresses[1].temporary = true;
        assert_eq!(ips(listed(&[wifi.clone()])), ["fdb2::1", "fe80::3"]);
        // Alone, it is the adapter's only way in over IPv6: it stays.
        wifi.addresses.remove(0);
        assert_eq!(ips(listed(&[wifi.clone()])), ["fdb2::2", "fe80::3"]);
        // Not yet checked for duplicates, or deprecated: left out.
        wifi.addresses[0].preferred = false;
        assert_eq!(ips(listed(&[wifi])), ["fe80::3"]);
    }

    #[test]
    fn this_machines_interfaces_read() {
        let listed = interface_ips().unwrap();
        assert!(
            listed
                .iter()
                .all(|ip| !matches!(ip, IpAddr::V4(ip) if ip.is_link_local())),
            "{listed:?}"
        );
    }

    #[test]
    fn netsh_is_told_the_rule_amux_asks_for() {
        let program = PathBuf::from(r"C:\Users\Some One\.amux\bin\amux.exe");
        let add = netsh_arguments(&program);
        assert!(
            add.starts_with(
                r#"advfirewall firewall add rule name="amux" dir=in action=allow program="C:\Users\Some One\.amux\bin\amux.exe" protocol=udp profile=private,domain enable=yes "#
            ),
            "{add}"
        );
        assert!(netsh().unwrap().ends_with(r"System32\netsh.exe"));
    }

    /// PowerShell is told to remove the one rule with the id, whatever the
    /// id holds: the script goes encoded, the id quoted inside it.
    #[test]
    fn powershell_is_told_the_one_rule_to_remove() {
        use base64::Engine as _;
        let id = r"UDP Query User{8A3C}C:\users\o'neil\.amux\bin\amux.exe";
        let arguments = powershell_arguments(id);
        let encoded = arguments
            .strip_prefix("-NoProfile -NonInteractive -EncodedCommand ")
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(
            script.contains(r"$id = 'UDP Query User{8A3C}C:\users\o''neil\.amux\bin\amux.exe'"),
            "{script}"
        );
        assert!(script.contains("-PolicyStore PersistentStore"), "{script}");
        assert!(script.contains("$_.Name -eq $id"), "{script}");
        assert!(
            script.ends_with("$rules | Remove-NetFirewallRule"),
            "{script}"
        );
        assert!(
            powershell()
                .unwrap()
                .ends_with(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        );
    }

    /// Reading changes nothing: it is all a test does with this machine's
    /// firewall.
    #[test]
    fn this_machines_firewall_reads() {
        let snapshot = Firewall.read().unwrap();
        assert!(!snapshot.rules.is_empty(), "Windows ships rules of its own");
        assert!(
            snapshot.rules.iter().all(|rule| !rule.id.is_empty()),
            "each rule read has the id it is kept under"
        );
        assert!(
            snapshot.rules.iter().any(|rule| !rule.policy),
            "Windows' own rules are the machine's"
        );
        let nobody = Path::new(r"C:\amux-test\no-such-program.exe");
        assert!(matches!(
            standing(&snapshot, nobody),
            Standing::Closed | Standing::Off
        ));
        Firewall.elevated().unwrap();
    }
}
