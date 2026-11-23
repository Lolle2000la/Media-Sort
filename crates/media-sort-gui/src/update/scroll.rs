use iced::Task;

use crate::message::Message;
use crate::state::AppState;
use crate::view::folder_panel::FOLDER_TREE_SCROLLABLE_ID;
use crate::view::media_grid::{
    MEDIA_GRID_CARD_SPACING, MEDIA_GRID_CARD_WIDTH, MEDIA_GRID_SCROLLABLE_ID,
};

/// Build a [`Task`] that scrolls the media grid so that the entry at `index`
/// is visible within the viewport with a comfortable margin (scroll padding).
pub fn scroll_to_selected_entry(state: &AppState, index: usize) -> Task<Message> {
    let total = state.media_grid.filtered_entries().len();
    if total <= 1 {
        return Task::none();
    }

    let clamped_index = index.min(total - 1);
    let card_stride = MEDIA_GRID_CARD_WIDTH + MEDIA_GRID_CARD_SPACING;
    let item_left = clamped_index as f32 * card_stride;
    let item_right = item_left + MEDIA_GRID_CARD_WIDTH;

    let scroll = &state.media_grid.scroll;

    let relative_x = if scroll.viewport_width > 0.0 {
        if scroll.content_width <= scroll.viewport_width {
            return Task::none();
        }

        let margin = card_stride * 1.5;
        let Some(target_offset) = calculate_scroll_into_view_1d(
            item_left,
            item_right,
            scroll.offset_x,
            scroll.viewport_width,
            scroll.content_width,
            margin,
        ) else {
            return Task::none();
        };

        let max_offset = scroll.content_width - scroll.viewport_width;
        (target_offset / max_offset).clamp(0.0, 1.0)
    } else {
        let Some(rel) = relative_position_for(clamped_index, total) else {
            return Task::none();
        };
        rel
    };

    iced::widget::operation::snap_to(
        MEDIA_GRID_SCROLLABLE_ID.clone(),
        iced::widget::scrollable::RelativeOffset {
            x: Some(relative_x),
            y: None,
        },
    )
}

/// Computes target 1D scroll offset to keep `[item_start, item_end]` visible within
/// `[current_offset, current_offset + viewport_size]` with at least `margin` padding.
/// Returns `Some(target_offset)` if scrolling is needed, or `None` if the item is
/// already comfortably in view.
pub fn calculate_scroll_into_view_1d(
    item_start: f32,
    item_end: f32,
    current_offset: f32,
    viewport_size: f32,
    content_size: f32,
    margin: f32,
) -> Option<f32> {
    if viewport_size <= 0.0 || content_size <= viewport_size {
        return None;
    }

    let max_offset = (content_size - viewport_size).max(0.0);
    let item_size = (item_end - item_start).max(0.0);
    let effective_margin = margin.min((viewport_size - item_size).max(0.0) / 2.0);

    let view_start = current_offset.clamp(0.0, max_offset);
    let view_end = view_start + viewport_size;

    let target_offset = if item_start - effective_margin < view_start {
        (item_start - effective_margin).max(0.0)
    } else if item_end + effective_margin > view_end {
        (item_end + effective_margin - viewport_size).min(max_offset)
    } else {
        return None;
    };

    if (target_offset - current_offset).abs() > 0.5 {
        Some(target_offset.clamp(0.0, max_offset))
    } else {
        None
    }
}

/// Compute the relative horizontal scroll position (in `[0.0, 1.0]`) that
/// corresponds to `index` in a list of `total` entries. Returns `None` when
/// the list has zero or one entries, in which case there is nothing to
/// scroll to.
pub fn relative_position_for(index: usize, total: usize) -> Option<f32> {
    if total <= 1 {
        return None;
    }
    let clamped_index = index.min(total - 1);
    Some(clamped_index as f32 / (total - 1) as f32)
}

pub fn scroll_to_selected_folder(state: &mut AppState) -> Task<Message> {
    let visible = state.folder.collect_visible_folders();
    let total = visible.len();
    let Some(idx) = state.folder.selected_folder_idx.filter(|i| *i < total) else {
        return Task::none();
    };

    if total <= 1 {
        return Task::none();
    }

    let scroll = &state.folder.scroll;

    let relative_y = if scroll.viewport_height > 0.0 {
        if scroll.content_height <= scroll.viewport_height {
            return Task::none();
        }

        let item_height = scroll.content_height / total as f32;
        let item_top = idx as f32 * item_height;
        let item_bottom = item_top + item_height;
        let margin = (scroll.viewport_height * 0.15).clamp(26.0, 78.0);

        let Some(target_offset) = calculate_scroll_into_view_1d(
            item_top,
            item_bottom,
            scroll.offset_y,
            scroll.viewport_height,
            scroll.content_height,
            margin,
        ) else {
            return Task::none();
        };

        let max_offset = scroll.content_height - scroll.viewport_height;
        (target_offset / max_offset).clamp(0.0, 1.0)
    } else {
        let Some(rel) = relative_position_for(idx, total) else {
            return Task::none();
        };
        rel
    };

    iced::widget::operation::snap_to(
        FOLDER_TREE_SCROLLABLE_ID.clone(),
        iced::widget::scrollable::RelativeOffset {
            x: None,
            y: Some(relative_y),
        },
    )
}
