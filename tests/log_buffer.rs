use herdr_tilt::logs::{LogBuffer, LogNavigation};

#[test]
fn log_buffer_bounds_scrollback_and_individual_line_size() {
    let mut logs = LogBuffer::with_limits(3, 8);

    logs.push("first");
    logs.push("second");
    logs.push("third");
    logs.push("a very long fourth line");

    assert_eq!(
        logs.lines().collect::<Vec<_>>(),
        ["second", "third", "a very l"]
    );
    assert_eq!(logs.len(), 3);
    assert_eq!(logs.dropped_lines(), 1);
    assert_eq!(logs.truncated_lines(), 1);
}

#[test]
fn log_buffer_preserves_a_scrolled_view_while_live_lines_arrive() {
    let mut logs = LogBuffer::with_limits(10, 80);
    for line in ["one", "two", "three", "four", "five"] {
        logs.push(line);
    }

    assert_eq!(logs.visible_lines(2), ["four", "five"]);
    assert!(logs.is_following());

    logs.navigate(LogNavigation::Up, 1);
    assert_eq!(logs.visible_lines(2), ["three", "four"]);
    assert!(!logs.is_following());

    logs.push("six");
    assert_eq!(logs.visible_lines(2), ["three", "four"]);

    logs.navigate(LogNavigation::End, 2);
    assert_eq!(logs.visible_lines(2), ["five", "six"]);
    assert!(logs.is_following());

    for _ in 0..20 {
        logs.navigate(LogNavigation::Up, 2);
    }
    assert_eq!(logs.visible_lines(2), ["one", "two"]);
}

#[test]
fn log_buffer_supports_follow_wrap_horizontal_navigation_and_clear() {
    let mut logs = LogBuffer::with_limits(10, 80);
    logs.push("one");
    logs.push("two");

    logs.toggle_follow();
    logs.push("three");
    assert_eq!(logs.visible_lines(2), ["one", "two"]);
    assert!(!logs.is_following());

    assert!(logs.is_wrapping());
    logs.toggle_wrap();
    let was_following = logs.is_following();
    logs.navigate(LogNavigation::Right, 10);
    assert_eq!(logs.horizontal_offset(), 1);
    assert_eq!(logs.is_following(), was_following);
    logs.navigate(LogNavigation::Left, 10);
    assert_eq!(logs.horizontal_offset(), 0);

    logs.toggle_follow();
    assert_eq!(logs.visible_lines(2), ["two", "three"]);
    logs.clear();
    assert!(logs.is_empty());
    assert!(logs.is_following());
}

#[test]
fn log_buffer_removes_terminal_escape_sequences_before_rendering() {
    let mut logs = LogBuffer::with_limits(10, 80);

    logs.push("\u{1b}[31merror\u{1b}[0m: failed");

    assert_eq!(logs.lines().collect::<Vec<_>>(), ["error: failed"]);
}
