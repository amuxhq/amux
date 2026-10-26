//! The profile verbs, on the front door.

use anyhow::Result;
use tonic::transport::Channel;
use wire::profile_service_client::ProfileServiceClient;
use wire::{CreateProfileRequest, DeleteProfileRequest, RenameProfileRequest};

use crate::connect;

type Profiles = ProfileServiceClient<Channel>;

pub async fn list(profiles: &mut Profiles) -> Result<()> {
    let listed = profiles
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .map_err(crate::plain)?
        .into_inner()
        .profiles;
    let width = listed
        .iter()
        .map(|profile| profile.label.len())
        .max()
        .unwrap_or(5)
        .max(5);
    println!("{:width$}  ID", "LABEL");
    for profile in listed {
        println!(
            "{:width$}  {}",
            profile.label,
            connect::profile_id(&profile)
        );
    }
    Ok(())
}

pub async fn create(profiles: &mut Profiles, label: Option<String>) -> Result<()> {
    let created = profiles
        .create_profile(CreateProfileRequest { label })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!(
        "Created profile {} ({}).",
        created.label,
        connect::profile_id(&created)
    );
    Ok(())
}

pub async fn rename(profiles: &mut Profiles, reference: &str, name: &str) -> Result<()> {
    let profile = connect::select(profiles, Some(reference)).await?;
    let renamed = profiles
        .rename_profile(RenameProfileRequest {
            profile_id: profile.id.clone(),
            override_name: Some(name.to_owned()),
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!("Renamed profile {} to {}.", profile.label, renamed.label);
    Ok(())
}

pub async fn delete(profiles: &mut Profiles, reference: &str) -> Result<()> {
    let profile = connect::select(profiles, Some(reference)).await?;
    profiles
        .delete_profile(DeleteProfileRequest {
            profile_id: profile.id.clone(),
        })
        .await
        .map_err(crate::plain)?;
    println!("Deleted profile {}.", profile.label);
    Ok(())
}
