//! A call's address. A host service is called on the host its authority
//! names, and the node a client is attached to passes a call addressed to
//! any other host on to it unread. The authority is the host's bare id, in
//! the one text form every client builds it in: lowercase, hyphenated, no
//! braces, no suffix and no port. Accepting that form only gives a host one
//! address, so two spellings of an id can never be routed differently.

use uuid::Uuid;

/// The authority a call to `host_id` is made with.
pub fn authority(host_id: &Uuid) -> String {
    host_id.hyphenated().to_string()
}

/// The host an authority addresses, when it is a host id in its canonical
/// form. Anything else, `amux.local` or a name included, addresses no
/// host, and the node the call reached answers it itself.
pub fn host_of_authority(authority: &str) -> Option<Uuid> {
    let host = Uuid::try_parse(authority).ok()?;
    let canonical = host.hyphenated().encode_lower(&mut Uuid::encode_buffer()) == authority;
    canonical.then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0192f0c8-7a51-7c3e-9d2f-3b1c5e6a7d80";

    #[test]
    fn a_host_id_round_trips_through_its_authority() {
        let host = Uuid::parse_str(ID).unwrap();
        assert_eq!(authority(&host), ID);
        assert_eq!(host_of_authority(&authority(&host)), Some(host));
    }

    /// Every other spelling of the same id, and anything that is not an
    /// id, addresses no host.
    #[test]
    fn only_the_canonical_form_addresses_a_host() {
        let refused = [
            ("uppercase", ID.to_uppercase()),
            ("simple", ID.replace('-', "")),
            ("braced", format!("{{{ID}}}")),
            ("urn", format!("urn:uuid:{ID}")),
            ("suffix", format!("{ID}.amux")),
            ("port", format!("{ID}:443")),
            ("name", "desk".to_owned()),
            ("local", "amux.local".to_owned()),
            ("empty", String::new()),
        ];
        for (form, authority) in refused {
            assert_eq!(host_of_authority(&authority), None, "{form}: {authority}");
        }
    }
}
