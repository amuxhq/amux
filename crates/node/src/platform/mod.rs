//! What node alone asks of the operating system, one file per OS, so the
//! rest of node writes no `cfg` for it: this boot of the machine, the
//! link socket's options, the interface addresses the machine is listed
//! at, and the firewall ([`firewall`]).

use std::io;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use std::net::IpAddr;

pub mod firewall;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
use linux as imp;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod macos;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use macos as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// This boot of the machine: changes at every reboot and at nothing else.
pub(crate) fn boot_id() -> io::Result<String> {
    imp::boot_id()
}

/// Makes the port the link's UDP socket is about to bind its own, so no
/// other program binds it too and takes the datagrams meant for amux.
///
/// Windows: SO_EXCLUSIVEADDRUSE, without which a socket that sets
/// SO_REUSEADDR may bind a port another holds. Elsewhere it does nothing,
/// and that is all it needs to do: a bound port is the binder's alone
/// unless both sockets ask to share it.
pub(crate) fn claim_port(socket: &socket2::Socket) -> io::Result<()> {
    #[cfg(windows)]
    return windows::claim_port(socket);
    #[cfg(not(windows))]
    {
        let _ = socket;
        Ok(())
    }
}

/// Makes a receive on the link's UDP socket wait for the next datagram
/// whatever an earlier send to a closed port brought back.
///
/// Windows: SIO_UDP_CONNRESET off, without which the ICMP port unreachable
/// is reported as WSAECONNRESET on the next receive. Elsewhere it does
/// nothing, and that is all it needs to do: an unconnected socket's
/// receive reports no ICMP error.
pub(crate) fn ignore_unreachable(socket: &std::net::UdpSocket) -> io::Result<()> {
    #[cfg(windows)]
    return windows::ignore_unreachable(socket);
    #[cfg(not(windows))]
    {
        let _ = socket;
        Ok(())
    }
}

/// The addresses of the machine's interfaces, of both families, that a
/// device on a network the machine is on could reach it at: what a LAN
/// listener on every interface is listed and advertised at, before the
/// callers' own filters (link-local IPv6, loopback where a peer elsewhere
/// is told).
///
/// Unix: every address getifaddrs reports. Windows: filtered as
/// [`windows::interface_ips`] says, which leaves out the tunnels, virtual
/// switches and stand-in addresses Windows reports beside the real ones.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub(crate) fn interface_ips() -> io::Result<Vec<IpAddr>> {
    #[cfg(windows)]
    return windows::interface_ips();
    #[cfg(not(windows))]
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| interface.ip())
        .collect())
}

/// This machine's firewall, where amux has one to set up: Windows'.
/// Elsewhere [`io::ErrorKind::Unsupported`].
pub(crate) fn firewall() -> io::Result<Box<dyn firewall::System>> {
    #[cfg(windows)]
    return Ok(Box::new(windows::Firewall));
    #[cfg(not(windows))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "amux sets up no firewall on this OS",
    ))
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    windows
)))]
mod imp {
    use std::io;

    pub fn boot_id() -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform has no boot id",
        ))
    }
}
