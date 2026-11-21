use jikji_core::PrepareOptions;

use crate::args::{BriefArgs, FindArgs, SearchArgs};
use crate::prepare_commands::normalize_max_files;

pub(crate) fn find_prepare_options(args: &FindArgs) -> PrepareOptions {
    PrepareOptions {
        include_hidden: args.include_hidden,
        include_sensitive: args.include_sensitive,
        max_files: normalize_max_files(args.max_files),
        exclude_patterns: args.exclude.clone(),
        max_hash_bytes: args.max_hash_bytes,
        parse_timeout_seconds: args.parse_timeout,
        skip_root_map: true,
        ..PrepareOptions::default()
    }
}

pub(crate) fn search_prepare_options(args: &SearchArgs) -> PrepareOptions {
    PrepareOptions {
        include_hidden: args.include_hidden,
        include_sensitive: args.include_sensitive,
        max_files: normalize_max_files(args.max_files),
        exclude_patterns: args.exclude.clone(),
        max_hash_bytes: args.max_hash_bytes,
        parse_timeout_seconds: args.parse_timeout,
        skip_root_map: true,
        ..PrepareOptions::default()
    }
}

pub(crate) fn brief_prepare_options(args: &BriefArgs) -> PrepareOptions {
    PrepareOptions {
        include_hidden: args.include_hidden,
        include_sensitive: args.include_sensitive,
        max_files: normalize_max_files(args.max_files),
        exclude_patterns: args.exclude.clone(),
        max_hash_bytes: args.max_hash_bytes,
        parse_timeout_seconds: args.parse_timeout,
        skip_root_map: true,
        ..PrepareOptions::default()
    }
}
