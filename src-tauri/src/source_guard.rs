//! For the tests that read this crate's own source.
//!
//! A few tests assert that a line is where it must be: a call without which a
//! feature is dead in the app while every test of its logic stays green. Such
//! a test has to drop comments first, or a call left in a comment counts as
//! made. Two modules strip comments, `sticky` and `placement`, and each had
//! its own idea of where a block comment ends. Both took the first `*/`.
//!
//! Rust block comments nest. Given `/* off /* for now */ call(); */` the
//! compiler reads one comment, and a stripper that stops at the first `*/`
//! keeps ` call(); */` as code. The fifth review of roadmap `1.47` put three
//! guarded calls inside such a comment, and all three guards stayed green.
//! The end of a block comment is decided here, once, for both.
//!
//! The comment forms Rust has, and where each is handled:
//!
//! - a line comment, `//` to the end of the line, which covers `///` and
//!   `//!`: each stripper cuts it itself;
//! - a block comment, `/*` to its matching `*/`, which covers `/**` and
//!   `/*!`, on one line or many, after code or on its own, nested or not:
//!   [`after_block_comment`].
//!
//! What no stripper sees is code made dead another way, such as a call inside
//! `if false { }` or under a `cfg` that is never set. These tests guard
//! against a line going missing. They are not a proof that the line runs.

/// What follows a block comment, given the text straight after its opening
/// `/*`. Counts nested openings as the compiler does, and returns nothing when
/// the comment never ends.
pub(crate) fn after_block_comment(inside: &str) -> &str {
    let bytes = inside.as_bytes();
    let mut depth = 1_usize;
    let mut at = 0;
    while at + 1 < bytes.len() {
        match (bytes[at], bytes[at + 1]) {
            (b'/', b'*') => {
                depth += 1;
                at += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                at += 2;
                if depth == 0 {
                    // Both marker bytes are ASCII, so this is a character
                    // boundary whatever the comment held.
                    return &inside[at..];
                }
            }
            _ => at += 1,
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::after_block_comment;

    #[test]
    fn a_block_comment_ends_at_its_own_end_and_not_at_an_inner_one() {
        assert_eq!(after_block_comment(" plain */ call();"), " call();");
        assert_eq!(after_block_comment("*/call();"), "call();");
        // The form the fifth review used.
        assert_eq!(
            after_block_comment(" off /* for now */ call(); */ kept();"),
            " kept();"
        );
        assert_eq!(
            after_block_comment(" a /* b /* c */ d */ e */ kept();"),
            " kept();"
        );
        // Across lines, and around text that is not ASCII.
        assert_eq!(
            after_block_comment("\n  /* ü */\n  call();\n*/\nkept();"),
            "\nkept();"
        );
    }

    #[test]
    fn a_block_comment_that_never_ends_takes_the_rest() {
        assert_eq!(after_block_comment(" never closed"), "");
        assert_eq!(after_block_comment(" outer /* inner */ call();"), "");
        assert_eq!(after_block_comment(""), "");
        assert_eq!(after_block_comment("*"), "");
    }

    #[test]
    fn the_markers_are_read_in_the_compilers_order() {
        // `/*/` opens a comment and leaves a slash. It does not also end one.
        assert_eq!(after_block_comment("/ still inside */ kept();"), " kept();");
        // A line comment marker means nothing inside a block comment.
        assert_eq!(after_block_comment(" // */ kept();"), " kept();");
    }
}
