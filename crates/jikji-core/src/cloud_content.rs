//! Which folders of a cloud-mounted drive may have their file *contents* indexed.
//!
//! A mounted cloud drive is slow and failure-prone, so by default `prepare` indexes only file
//! names, paths, sizes and times there. The user can approve one folder at a time
//! (`jikji prepare FOLDER --approve-cloud-content`); that approval is stored in the central
//! database, covers the folder and everything below it, and is honoured by every later `prepare`
//! or `refresh` of that folder or of a parent mount, which then reads the contents of the
//! approved folder only. Folders indexed with contents before this policy existed are kept
//! (`origin = existing_index`) so a refresh does not silently drop their text.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::storage::{cloud_drive_path_blocked, open_database, root_id};
use crate::{Result, io_error, storage::database_path};

const STATUS_APPROVED: &str = "approved";
const STATUS_REVOKED: &str = "revoked";

/// One stored decision about a cloud folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloudApproval {
    pub folder: PathBuf,
    /// `approved` or `revoked`.
    pub status: String,
    /// `user` (explicit flag) or `existing_index` (kept from before the policy).
    pub origin: String,
}

/// Content-indexing decision for one root, loaded once per `prepare`/search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloudContentPolicy {
    cloud: bool,
    approved: Vec<PathBuf>,
}

impl CloudContentPolicy {
    /// A policy that allows contents everywhere (any path that is not on a cloud drive).
    pub fn unrestricted() -> Self {
        Self {
            cloud: false,
            approved: Vec::new(),
        }
    }

    /// Policy for `root`; `also` are other spellings of the same root (the typed path before
    /// symlinks were resolved), because a cloud mount may only be recognisable by those.
    pub fn load(root: &Path, also: &[&Path]) -> Result<Self> {
        let cloud = cloud_drive_path_blocked(root)
            || also.iter().any(|path| cloud_drive_path_blocked(path));
        if !cloud {
            return Ok(Self::unrestricted());
        }
        let approved = list_approvals()?
            .into_iter()
            .filter(|approval| approval.status == STATUS_APPROVED)
            .map(|approval| approval.folder)
            .collect();
        Ok(Self { cloud, approved })
    }

    /// Policy when the stored decisions cannot be read: on a cloud drive nothing is approved
    /// (names only), elsewhere nothing is restricted.
    pub fn load_or_deny(root: &Path) -> Self {
        Self::load(root, &[]).unwrap_or_else(|_| Self {
            cloud: cloud_drive_path_blocked(root),
            approved: Vec::new(),
        })
    }

    pub fn is_cloud(&self) -> bool {
        self.cloud
    }

    /// May the contents of `path` be read and indexed?
    pub fn allows(&self, path: &Path) -> bool {
        if self.cloud {
            return self.approved.iter().any(|folder| path.starts_with(folder));
        }
        // A symlinked file in an ordinary folder may lead into a cloud drive; only then look at
        // where it really points, so ordinary files cost no extra system call chain.
        let is_link =
            std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
        !is_link || !cloud_drive_path_blocked(path)
    }

    /// Approved folders inside `root` (not covering it): where text can be searched below a
    /// filename-only mount.
    pub fn approved_below(&self, root: &Path) -> Vec<PathBuf> {
        let mut below: Vec<PathBuf> = self
            .approved
            .iter()
            .filter(|folder| folder.starts_with(root) && *folder != root)
            .cloned()
            .collect();
        below.sort();
        below
    }

    /// `full` (not a cloud drive), `approved_folder` (this root is covered) or `filename_only`.
    pub fn mode_for(&self, root: &Path) -> &'static str {
        if !self.cloud {
            "full"
        } else if self.allows(root) {
            "approved_folder"
        } else {
            "filename_only"
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

fn ensure_table(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS cloud_content_approvals(
                folder TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                origin TEXT NOT NULL,
                decided_at INTEGER NOT NULL
            );",
        )
        .map_err(db_error)
}

fn db_error(source: rusqlite::Error) -> crate::JikjiError {
    crate::storage::sqlite_error(
        &database_path().unwrap_or_else(|_| PathBuf::from("index.sqlite")),
        source,
    )
}

/// Every stored decision, approved and revoked, ordered by folder.
pub fn list_approvals() -> Result<Vec<CloudApproval>> {
    let connection = open_database()?;
    ensure_table(&connection)?;
    let mut statement = connection
        .prepare("SELECT folder, status, origin FROM cloud_content_approvals ORDER BY folder")
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(CloudApproval {
                folder: PathBuf::from(row.get::<_, String>(0)?),
                status: row.get(1)?,
                origin: row.get(2)?,
            })
        })
        .map_err(db_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)
}

fn write_decision(folder: &Path, status: &str, origin: &str, replace: bool) -> Result<()> {
    let connection = open_database()?;
    ensure_table(&connection)?;
    let sql = if replace {
        "INSERT INTO cloud_content_approvals(folder,status,origin,decided_at) VALUES(?1,?2,?3,?4)
         ON CONFLICT(folder) DO UPDATE SET status=excluded.status, origin=excluded.origin, decided_at=excluded.decided_at"
    } else {
        "INSERT OR IGNORE INTO cloud_content_approvals(folder,status,origin,decided_at) VALUES(?1,?2,?3,?4)"
    };
    connection
        .execute(
            sql,
            params![folder.to_string_lossy(), status, origin, now()],
        )
        .map_err(db_error)?;
    Ok(())
}

/// Approve content indexing of `folder` (canonical path) and everything below it.
pub fn approve_cloud_content(folder: &Path) -> Result<()> {
    write_decision(folder, STATUS_APPROVED, "user", true)
}

/// Withdraw the approval of exactly `folder`. Returns whether a decision for it existed.
/// The next `prepare` of the folder (or a parent mount) indexes file names only again.
pub fn revoke_cloud_content(folder: &Path) -> Result<bool> {
    let connection = open_database()?;
    ensure_table(&connection)?;
    let existed = connection
        .query_row(
            "SELECT 1 FROM cloud_content_approvals WHERE folder=?1",
            [folder.to_string_lossy()],
            |_| Ok(()),
        )
        .optional()
        .map_err(db_error)?
        .is_some();
    write_decision(folder, STATUS_REVOKED, "user", true)?;
    Ok(existed)
}

/// Keep the contents of a cloud root that was indexed with text before this policy existed:
/// when `root` has no decision of its own and its index holds text, record it as approved.
/// Returns whether it did. Must run before the new index replaces the old one.
pub fn keep_existing_cloud_content(root: &Path) -> Result<bool> {
    let connection = open_database()?;
    ensure_table(&connection)?;
    let decided = connection
        .query_row(
            "SELECT 1 FROM cloud_content_approvals WHERE folder=?1",
            [root.to_string_lossy()],
            |_| Ok(()),
        )
        .optional()
        .map_err(db_error)?
        .is_some();
    if decided {
        return Ok(false);
    }
    let Some(id) = root_id(&connection, root)? else {
        return Ok(false);
    };
    // `search_docs` exists once the root was indexed; a missing table means nothing to keep.
    let has_text = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM search_docs WHERE root_id=?1 AND COALESCE(body_size,0)>0)",
            [id],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false);
    if !has_text {
        return Ok(false);
    }
    write_decision(root, STATUS_APPROVED, "existing_index", false)?;
    Ok(true)
}

/// Canonical form used for stored decisions; errors when `folder` is not a directory.
pub fn approval_key(folder: &Path) -> Result<PathBuf> {
    let canonical = crate::storage::canonical_root(folder)?;
    if !canonical.is_dir() {
        return Err(io_error(
            folder,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a directory"),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::CloudContentPolicy;

    #[test]
    fn a_policy_that_is_not_on_a_cloud_drive_allows_everything() {
        let policy = CloudContentPolicy::unrestricted();
        assert!(policy.allows(Path::new("/home/u/docs/a.pdf")));
        assert_eq!(policy.mode_for(Path::new("/home/u/docs")), "full");
    }

    #[test]
    fn approved_folders_cover_their_subtree_and_nothing_else() {
        let policy = CloudContentPolicy {
            cloud: true,
            approved: vec!["/m/GoogleDrive/docs".into()],
        };
        assert!(policy.allows(Path::new("/m/GoogleDrive/docs/a/b.pdf")));
        assert!(policy.allows(Path::new("/m/GoogleDrive/docs")));
        assert!(!policy.allows(Path::new("/m/GoogleDrive/docs2/b.pdf")));
        assert!(!policy.allows(Path::new("/m/GoogleDrive/other/b.pdf")));
        assert_eq!(
            policy.mode_for(Path::new("/m/GoogleDrive")),
            "filename_only"
        );
        assert_eq!(
            policy.mode_for(Path::new("/m/GoogleDrive/docs/a")),
            "approved_folder"
        );
        assert_eq!(
            policy.approved_below(Path::new("/m/GoogleDrive")),
            [Path::new("/m/GoogleDrive/docs")]
        );
        assert!(
            policy
                .approved_below(Path::new("/m/GoogleDrive/docs"))
                .is_empty()
        );
    }
}
