//! Conversation naming: what counts as a placeholder, and the provisional title
//! a fresh session carries until the model names it.

use thunder_conversation::prelude::*;

#[test]
fn placeholder_titles_cover_the_seeds_and_truncations() {
    // Never named.
    assert!(Conversation::new("s1").is_title_placeholder());
    // The seed a fresh TUI session carries.
    assert!(Conversation::new("s1")
        .with_title("New Conversation")
        .is_title_placeholder());
    assert!(Conversation::new("s1")
        .with_title("untitled")
        .is_title_placeholder());
    assert!(Conversation::new("s1")
        .with_title("   ")
        .is_title_placeholder());
    // The truncated first-prompt title the daemon writes.
    assert!(Conversation::new("s1")
        .with_title("[Active Workspace: /Users/luca...")
        .is_title_placeholder());
    // A real title is not replaceable.
    assert!(!Conversation::new("s1")
        .with_title("真正的标题")
        .is_title_placeholder());
    assert!(!Conversation::new("s1")
        .with_title("Refactor the prompt box")
        .is_title_placeholder());
}

#[test]
fn a_long_first_prompt_becomes_a_replaceable_provisional_title() {
    let prompt = "Please refactor the prompt box so that it grows with the row count";
    let title = provisional_title(prompt);

    assert_eq!(title, "Please refactor the prompt box...");
    assert_eq!(title.chars().count(), 33);
    assert!(Conversation::new("s1")
        .with_title(title)
        .is_title_placeholder());
}

#[test]
fn a_short_first_prompt_is_the_title_verbatim() {
    let title = provisional_title("fix the parser");

    assert_eq!(title, "fix the parser");
    assert!(!Conversation::new("s1")
        .with_title(title)
        .is_title_placeholder());
}

#[test]
fn a_multiline_prompt_flattens_onto_one_line() {
    let title = provisional_title("audit the\n  store   layer\n");

    assert_eq!(title, "audit the store layer");
}
