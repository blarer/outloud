//! Schema migration: read old config files forever, decided now.
//!
//! The contract, fixed before any user has a file so it never has to change
//! under them:
//!
//! - Every file we write carries `schema-version = N` (currently 1). A file
//!   without one is treated as the current version, which is correct for
//!   hand-made files and harmless otherwise.
//! - Renames/removals/retypes bump [`crate::schema::SCHEMA_VERSION`] and add
//!   one [`Step`] here. Steps run in order, each transforming version N to
//!   N+1, so a version-1 file always has a path to current.
//! - Migration edits the *parsed document*, preserving the user's comments
//!   and layout, and only rewrites the file after a successful transform.
//!   The pre-migration file is kept as `config.toml.v<N>` so a downgrade or
//!   a migration bug never costs the user their settings.
//! - Files from the *future* (version > current) are read best-effort and
//!   never rewritten: a newer binary's file is not ours to "fix".

use toml_edit::DocumentMut;

use crate::schema::SCHEMA_VERSION;

/// One version-to-version transformation.
struct Step {
    /// The version this step migrates FROM (produces `from + 1`).
    from: i64,
    /// What changed, quoted in logs so the user can see what happened.
    description: &'static str,
    apply: fn(&mut DocumentMut),
}

/// The migration chain. Empty today because version 1 is the first; the
/// commented example documents the pattern the first real migration follows.
fn steps() -> &'static [Step] {
    // Example future entry:
    // Step {
    //     from: 1,
    //     description: "rename `hotkey` to `bindings.push-to-talk`",
    //     apply: |doc| {
    //         if let Some(v) = doc.remove("hotkey") {
    //             doc["bindings"]["push-to-talk"] = v;
    //         }
    //     },
    // },
    &[]
}

/// The outcome of migrating one document.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Already current (or version-less, treated as current). No rewrite.
    Current,
    /// Migrated from `from` to the current version; the caller should write
    /// `text` back and keep the original as `config.toml.v<from>`. `applied`
    /// lists each step's description for the log, so "my config changed"
    /// has a written explanation.
    Upgraded {
        from: i64,
        text: String,
        applied: Vec<&'static str>,
    },
    /// Written by a newer binary. Read best-effort, never rewritten.
    FromFuture { found: i64 },
    /// The declared version is older than anything we know how to migrate
    /// (cannot happen until versions are retired, but the case is named now
    /// so retiring one later is an explicit decision).
    TooOld { found: i64 },
}

/// Migrate a config document to the current schema version.
///
/// Pure: takes text, returns text. The caller owns file I/O and the backup,
/// keeping every path here testable.
pub fn migrate(text: &str) -> Result<Migration, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("cannot migrate: not valid TOML: {e}"))?;

    let found = doc
        .get("schema-version")
        .and_then(|i| i.as_integer())
        .unwrap_or(SCHEMA_VERSION);

    if found == SCHEMA_VERSION {
        return Ok(Migration::Current);
    }
    if found > SCHEMA_VERSION {
        return Ok(Migration::FromFuture { found });
    }
    let oldest_supported = steps().first().map_or(SCHEMA_VERSION, |s| s.from);
    if found < oldest_supported {
        return Ok(Migration::TooOld { found });
    }

    let mut version = found;
    let mut applied = Vec::new();
    for step in steps() {
        if step.from == version {
            (step.apply)(&mut doc);
            applied.push(step.description);
            version += 1;
        }
    }
    debug_assert_eq!(version, SCHEMA_VERSION, "gap in the migration chain");
    doc["schema-version"] = toml_edit::value(SCHEMA_VERSION);
    Ok(Migration::Upgraded {
        from: found,
        text: doc.to_string(),
        applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_needs_nothing() {
        assert_eq!(
            migrate("schema-version = 1\nhotkey = \"fn\"\n").unwrap(),
            Migration::Current
        );
    }

    #[test]
    fn versionless_file_is_treated_as_current() {
        // Hand-made files rarely declare a version; punishing that would
        // punish exactly the users the plain-text format exists for.
        assert_eq!(migrate("hotkey = \"fn\"\n").unwrap(), Migration::Current);
    }

    #[test]
    fn future_version_is_left_alone() {
        assert_eq!(
            migrate("schema-version = 2\n").unwrap(),
            Migration::FromFuture { found: 2 }
        );
    }

    #[test]
    fn zero_or_negative_versions_are_too_old() {
        assert_eq!(
            migrate("schema-version = 0\n").unwrap(),
            Migration::TooOld { found: 0 }
        );
    }

    #[test]
    fn malformed_toml_cannot_be_migrated() {
        let err = migrate("schema-version = \"one").unwrap_err();
        assert!(err.contains("not valid TOML"), "{err}");
    }
}
