//! Package domain: catalog model, intake (fetch / TTL / mirror), asset
//! selection + download + archive extraction, version-spec resolution,
//! payload identification, and user-facing name matching. The public API
//! is re-exported here so callers keep using `crate::packages::*` paths.

pub mod archive;
pub mod assets;
pub mod catalog;
pub mod download;
pub mod fetch;
pub mod home;
pub mod identify;
pub mod names;
pub mod version;

pub use archive::{extract_zip, sanitize_entry};
pub use assets::select_asset_url;
pub use catalog::{
    LockFile, LockedAsset, LockedFile, PackageEntry, PackagesFile, PostInstall, VersionEntry,
    existent_slot, slot_ids, version_covers_game,
};
pub use download::{download, fetch_asset, store_root};
pub use fetch::{
    SUPPORTED_LOCK_SCHEMA, SUPPORTED_PACKAGES_SCHEMA, get_lock, get_packages, load_metadata,
};
pub use home::{chef_home, clear_home_override, set_home_override};
pub use identify::{identify_digests, payload_basenames, payload_index};
pub use names::{
    NameMatch, disambiguate, levenshtein, narrow_by_game, normalize, resolve, resolve_id,
};
pub(crate) use version::parse_version_loose;
pub use version::{
    ResolvedVersion, available_versions, display_version, list_version, resolve_spec, version_word,
};
