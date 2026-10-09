//! Built-in random values for the `${...}` templating engine.
//!
//! A placeholder whose name starts with `random.` is a *built-in*: when no
//! scope supplies that name, the engine asks this module for a fresh value.
//! Ordinary lookup runs first, so the session store and the environment can
//! shadow a built-in — a captured `random.int.6` makes a run reproducible,
//! which is also how the unit tests stay deterministic.
//!
//! | name | value |
//! |---|---|
//! | `random.string` | 16 random ASCII letters or digits |
//! | `random.string.<n>` | `n` random ASCII letters or digits (`1`–`1024`) |
//! | `random.int` | a random integer in `0..=4294967295` |
//! | `random.int.<max>` | a random integer in `0..=max` |
//! | `random.int.<min>.<max>` | a random integer in `min..=max` |
//! | `random.bool` | `true` or `false` |
//!
//! A whole-placeholder use keeps its JSON type — `${random.int}` is a number
//! and `${random.bool}` a boolean — while inside a longer string every value
//! uses its string form like any other variable.

use rand::Rng;
use serde_json::Value;

/// The reserved namespace; names starting with it are never plain variables.
pub const PREFIX: &str = "random.";

/// Length of `random.string` when no explicit length is given.
const DEFAULT_STRING_LEN: usize = 16;

/// Largest length `random.string.<n>` may ask for.
const MAX_STRING_LEN: usize = 1024;

/// Upper bound (inclusive) of `random.int` when no range is given.
const DEFAULT_INT_MAX: i64 = u32::MAX as i64;

/// Characters `random.string` draws from: ASCII letters and digits.
const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Why a `random.` name could not be turned into a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RandomError {
    /// The name is in the reserved namespace but is not a known generator.
    #[error("unknown random value `{0}`")]
    Unknown(String),
    /// A known generator was given arguments it does not accept.
    #[error("invalid random value `{name}`: {reason}")]
    Argument {
        /// The name as written in the template.
        name: String,
        /// What is wrong with its arguments.
        reason: String,
    },
}

/// Generates the built-in value named by `name`.
///
/// Returns `None` when `name` does not start with [`PREFIX`], telling the
/// caller to report the usual "undefined variable". Any name inside the
/// namespace returns `Some`, with `Err` when it does not describe a generator.
pub fn generate(name: &str) -> Option<Result<Value, RandomError>> {
    let rest = name.strip_prefix(PREFIX)?;
    let mut parts = rest.split('.');
    let kind = parts.next().unwrap_or_default();
    let args: Vec<&str> = parts.collect();
    let result = match kind {
        "string" => match args.as_slice() {
            [] => Ok(random_string(DEFAULT_STRING_LEN)),
            [len] => parse_i64(len)
                .and_then(|len| match usize::try_from(len) {
                    Ok(len) if (1..=MAX_STRING_LEN).contains(&len) => Ok(random_string(len)),
                    _ => Err(format!("length must be between 1 and {MAX_STRING_LEN}")),
                })
                .map_err(|reason| invalid(name, reason)),
            _ => Err(invalid(name, "expected no argument or one length")),
        },
        "int" => match args.as_slice() {
            [] => Ok(random_int(0, DEFAULT_INT_MAX)),
            [max] => parse_i64(max)
                .and_then(|max| {
                    if max < 0 {
                        Err("maximum must be >= 0".to_string())
                    } else {
                        Ok(random_int(0, max))
                    }
                })
                .map_err(|reason| invalid(name, reason)),
            [min, max] => parse_i64(min)
                .and_then(|min| parse_i64(max).map(|max| (min, max)))
                .and_then(|(min, max)| {
                    if min <= max {
                        Ok(random_int(min, max))
                    } else {
                        Err(format!("minimum {min} is greater than maximum {max}"))
                    }
                })
                .map_err(|reason| invalid(name, reason)),
            _ => Err(invalid(
                name,
                "expected no argument, a maximum, or a minimum and a maximum",
            )),
        },
        "bool" => match args.as_slice() {
            [] => Ok(Value::Bool(rand::random_bool(0.5))),
            _ => Err(invalid(name, "expected no argument")),
        },
        _ => Err(RandomError::Unknown(name.to_string())),
    };
    Some(result)
}

fn invalid(name: &str, reason: impl Into<String>) -> RandomError {
    RandomError::Argument {
        name: name.to_string(),
        reason: reason.into(),
    }
}

fn random_string(len: usize) -> Value {
    let mut rng = rand::rng();
    let text: String = (0..len)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect();
    Value::String(text)
}

fn random_int(min: i64, max: i64) -> Value {
    Value::Number(rand::rng().random_range(min..=max).into())
}

/// Parses an integer argument, rejecting an empty one with a clear reason.
fn parse_i64(text: &str) -> Result<i64, String> {
    text.parse()
        .map_err(|_| format!("`{text}` is not an integer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(name: &str) -> Value {
        generate(name)
            .unwrap_or_else(|| panic!("`{name}` is not in the reserved namespace"))
            .unwrap_or_else(|error| panic!("`{name}` failed: {error}"))
    }

    #[test]
    fn only_the_reserved_namespace_is_handled() {
        assert!(generate("token").is_none());
        assert!(generate("random").is_none());
        assert!(generate("randomish.string").is_none());
        assert!(generate("random.").is_some());
    }

    #[test]
    fn strings_default_to_sixteen_alphanumeric_characters() {
        let value = value("random.string");
        let text = value.as_str().expect("random.string is a string");
        assert_eq!(text.len(), DEFAULT_STRING_LEN);
        assert!(text.chars().all(|c| c.is_ascii_alphanumeric()), "{text}");
    }

    #[test]
    fn string_length_is_configurable() {
        for len in [1usize, 7, MAX_STRING_LEN] {
            let text = value(&format!("random.string.{len}"));
            assert_eq!(text.as_str().map(str::len), Some(len));
        }
    }

    #[test]
    fn bad_string_lengths_are_rejected() {
        for name in [
            "random.string.0",
            "random.string.1025",
            "random.string.x",
            "random.string.1.2",
        ] {
            assert!(
                matches!(generate(name), Some(Err(RandomError::Argument { .. }))),
                "{name}"
            );
        }
    }

    #[test]
    fn integers_stay_inside_their_range() {
        for _ in 0..100 {
            let n = value("random.int.1.6").as_u64().expect("an integer");
            assert!((1..=6).contains(&n));
        }
        assert_eq!(value("random.int.5.5"), serde_json::json!(5));
        for _ in 0..100 {
            let n = value("random.int.4").as_i64().expect("an integer");
            assert!((0..=4).contains(&n));
            let n = value("random.int.-3.3").as_i64().expect("an integer");
            assert!((-3..=3).contains(&n));
        }
    }

    #[test]
    fn bad_integer_ranges_are_rejected() {
        for name in [
            "random.int.10.5",
            "random.int.-1",
            "random.int.x",
            "random.int.1.2.3",
        ] {
            assert!(
                matches!(generate(name), Some(Err(RandomError::Argument { .. }))),
                "{name}"
            );
        }
    }

    #[test]
    fn booleans_are_booleans() {
        for _ in 0..20 {
            assert!(value("random.bool").is_boolean());
        }
        assert!(matches!(
            generate("random.bool.true"),
            Some(Err(RandomError::Argument { .. }))
        ));
    }

    #[test]
    fn unknown_generators_name_themselves() {
        assert_eq!(
            generate("random.nope"),
            Some(Err(RandomError::Unknown("random.nope".to_string())))
        );
        assert!(generate("random.").is_some());
    }
}
