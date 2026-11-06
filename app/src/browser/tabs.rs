//! Which browser tab is selected after one closes.

/// The index of the tab to select after closing the tab at `closed` out of `count` tabs while the
/// one at `active` is selected: the same tab when another one closes, else the neighbour that
/// takes its place (or the one before it, for the last tab). `None` when no tab is left.
pub fn active_after_close(count: usize, active: usize, closed: usize) -> Option<usize> {
    let remaining = count.checked_sub(1).filter(|remaining| *remaining > 0)?;
    let active = if closed < active {
        active - 1
    } else if closed == active {
        closed
    } else {
        active
    };
    Some(active.min(remaining - 1))
}

#[cfg(test)]
#[path = "tabs_tests.rs"]
mod tests;
