//! Images Claude made or showed: thumbnails under the tool calls that produced them and previews
//! of the local images its answers embed, all opening in the lightbox.

use std::path::Path;

use warpui::Element;
use warpui::assets::asset_cache::AssetSource;
use warpui::elements::{
    Border, Clipped, ConstrainedBox, Container, CornerRadius, Flex, Hoverable, Image, MainAxisSize,
    ParentElement, Radius, Shrinkable, Text, Wrap,
};
use warpui::image_cache::CacheOption;
use warpui::platform::Cursor;

use super::TranscriptAction;
use super::paths::PathKind;
use super::render::RowContext;

/// Tool images show as square crops of this side, small enough to keep a work group compact.
const TOOL_THUMBNAIL_SIZE: f32 = 64.;
const TOOL_THUMBNAIL_GAP: f32 = 6.;
const TOOL_THUMBNAIL_RADIUS: f32 = 6.;
/// Thumbnails shown in one strip; the lightbox still has all of them.
const MAX_TOOL_THUMBNAILS: usize = 12;
/// An embedded image's preview box, which the image fits in without cropping.
const PREVIEW_MAX_WIDTH: f32 = 420.;
const PREVIEW_HEIGHT: f32 = 240.;
const PREVIEW_RADIUS: f32 = 8.;

/// An image ready to show: where it loads from and the file name to caption it with.
#[derive(Debug, Clone)]
pub(super) struct ShownImage {
    pub source: AssetSource,
    pub name: Option<String>,
}

/// Small thumbnails of `images` in rows that wrap; clicking the one at a position dispatches
/// `open(position)`. `key` tells strips apart.
pub(super) fn image_strip(
    cx: &RowContext,
    key: &str,
    images: &[ShownImage],
    open: impl Fn(usize) -> TranscriptAction,
) -> Box<dyn Element> {
    let palette = cx.palette();
    let radius = CornerRadius::with_all(Radius::Pixels(TOOL_THUMBNAIL_RADIUS));
    let mut strip = Wrap::row()
        .with_spacing(TOOL_THUMBNAIL_GAP)
        .with_run_spacing(TOOL_THUMBNAIL_GAP)
        .with_main_axis_size(MainAxisSize::Min);
    for (position, image) in images.iter().take(MAX_TOOL_THUMBNAILS).enumerate() {
        let tile = ConstrainedBox::new(
            Container::new(
                Clipped::new(
                    // `BySize` crops a cover-fit image to the box; `Original` would paint past it.
                    Image::new(image.source.clone(), CacheOption::BySize)
                        .cover()
                        .with_corner_radius(radius)
                        .finish(),
                )
                .finish(),
            )
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(palette.border.0))
            .with_corner_radius(radius)
            .finish(),
        )
        .with_width(TOOL_THUMBNAIL_SIZE)
        .with_height(TOOL_THUMBNAIL_SIZE)
        .finish();
        let action = open(position);
        strip.add_child(
            Hoverable::new(
                cx.view.mouse(&format!("tool_image:{key}:{position}")),
                move |_| tile,
            )
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .finish(),
        );
    }
    strip.finish()
}

/// An image an answer embeds with `![alt](source)`: a preview of the file when it's there, else
/// its description.
pub(super) fn markdown_image(
    cx: &RowContext,
    key: &str,
    source: &str,
    alt: &str,
    path: Option<&Path>,
) -> Box<dyn Element> {
    let palette = cx.palette();
    let Some(path) = path else {
        let label = if alt.trim().is_empty() { source } else { alt };
        return Text::new(label.to_owned(), cx.style.ui, cx.sizes().small_font_size)
            .with_color(palette.text_muted.0)
            .with_selectable(false)
            .finish();
    };
    let radius = CornerRadius::with_all(Radius::Pixels(PREVIEW_RADIUS));
    let preview = ConstrainedBox::new(
        Container::new(
            Clipped::new(
                Image::new(
                    AssetSource::LocalFile {
                        path: path.to_string_lossy().into_owned(),
                        content_version: None,
                    },
                    CacheOption::BySize,
                )
                .contain()
                .finish(),
            )
            .finish(),
        )
        .with_uniform_padding(4.)
        .with_background_color(palette.surface.0)
        .with_border(Border::all(1.).with_border_color(palette.border.0))
        .with_corner_radius(radius)
        .finish(),
    )
    .with_max_width(PREVIEW_MAX_WIDTH)
    .with_height(PREVIEW_HEIGHT)
    .finish();
    let action = TranscriptAction::OpenPath {
        path: path.to_path_buf(),
        kind: PathKind::Image,
    };
    let preview = Hoverable::new(cx.view.mouse(&format!("image:{key}")), move |_| preview)
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish();
    // In a row, so the preview can be narrower than the answer it sits in.
    Flex::row()
        .with_child(Shrinkable::new(1., preview).finish())
        .finish()
}
