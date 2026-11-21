# Cloud-mounted drives: file names by default, contents per approved folder

Issue: Cheol-H-Jeong/jikji#1. A path with a component named `GoogleDrive`, `Google Drive`, `gdrive`
(case, space, `-`, `_` ignored; symlinks followed one level, see `cloud_drive_path_blocked`) is a
cloud mount.

| rule | behaviour |
|---|---|
| default | `prepare` indexes names, paths, sizes, times; no file is parsed or read for text |
| approve | `jikji prepare FOLDER --approve-cloud-content` stores the decision (table `cloud_content_approvals` in the central database) and indexes that folder with contents |
| scope | an approval covers the folder and everything below it |
| persistence | every later `prepare`/`refresh` of the folder or of a parent mount reads contents of approved folders only |
| revoke | `--revoke-cloud-content` withdraws exactly that folder's approval and rebuilds it name-only |
| existing indexes | a cloud root indexed with text by an older release is recorded once as `origin=existing_index` on its next `prepare`, so a refresh keeps its text |
| ordinary folders | unchanged; `--approve-cloud-content` on a non-cloud path is refused |

`jikji find` reports `cloud_content.mode` (`filename_only`, `approved_folder`,
`indexed_before_policy`), `approved_folders_below[]`, and note `cloud_filename_only` when contents
are not searchable. A cloud mount is never walked to check freshness (`freshness_not_checked`);
`jikji refresh` the folder after adding files. Code: `crates/jikji-core/src/cloud_content.rs`;
tests: `crates/jikji-cli/tests/cloud_content_policy.rs`.
