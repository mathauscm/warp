use super::*;

#[test]
fn an_empty_file_keeps_the_defaults() {
    assert_eq!(
        DesignTokens::from_toml("").unwrap(),
        DesignTokens::default()
    );
}

#[test]
fn overrides_replace_only_the_keys_they_name() {
    let tokens = DesignTokens::from_toml(
        r##"
        [light]
        accent = "#112233"
        [sizes]
        font_size = 16.0
        "##,
    )
    .unwrap();

    assert_eq!(tokens.light.accent, Hex::rgb(0x112233));
    assert_eq!(tokens.light.page, Palette::light().page);
    assert_eq!(tokens.dark, Palette::dark());
    assert_eq!(tokens.sizes.font_size, 16.);
    assert_eq!(
        tokens.sizes.column_max_width,
        Sizes::default().column_max_width
    );
}

#[test]
fn colors_take_an_optional_alpha() {
    assert_eq!(Hex::parse("#d97757"), Some(Hex::rgb(0xd97757)));
    assert_eq!(Hex::parse("#00000059"), Some(Hex::rgba(0x00000059)));
    assert_eq!(Hex::parse("d97757"), None);
    assert_eq!(Hex::parse("#d977"), None);
}

#[test]
fn a_bad_color_is_an_error() {
    assert!(DesignTokens::from_toml("[dark]\naccent = \"orange\"").is_err());
}

fn theme(background: u32, foreground: u32, accent: u32) -> WarpTheme {
    use warp_core::ui::theme::Fill;

    let base = crate::themes::default_themes::dark_theme();
    WarpTheme::new(
        Fill::Solid(Hex::rgb(background).0),
        Hex::rgb(foreground).0,
        Fill::Solid(Hex::rgb(accent).0),
        None,
        None,
        base.terminal_colors().clone(),
        None,
        None,
    )
}

#[test]
fn a_dark_warp_theme_gives_its_colors_to_the_chat() {
    let palette = Palette::from_warp_theme(&theme(0x101820, 0xf0f0f0, 0x19aad8));

    assert_eq!(palette.page, Hex::rgb(0x101820));
    assert_eq!(palette.text, Hex::rgb(0xf0f0f0));
    // Surfaces and borders step from the background toward the foreground.
    let lightness = |hex: Hex| hex.0.r as u32 + hex.0.g as u32 + hex.0.b as u32;
    assert!(lightness(palette.page) < lightness(palette.surface));
    assert!(lightness(palette.surface) < lightness(palette.surface_hover));
    assert!(lightness(palette.border) < lightness(palette.border_strong));
    assert!(lightness(palette.text_faint) < lightness(palette.text_muted));
    assert!(lightness(palette.text_muted) < lightness(palette.text));
    assert_eq!(palette.success, Palette::dark().success);
}

#[test]
fn a_light_warp_theme_gives_a_light_chat() {
    let palette = Palette::from_warp_theme(&theme(0xffffff, 0x111111, 0x00c2ff));

    assert_eq!(palette.page, Hex::rgb(0xffffff));
    assert_eq!(palette.text, Hex::rgb(0x111111));
    assert!(palette.surface.0.r < 0xff);
    assert_eq!(palette.success, Palette::light().success);
}

#[test]
fn the_default_accent_is_blue_not_orange() {
    let warp = theme(0x101820, 0xf0f0f0, 0x19aad8);
    let accent = accent_color(ClaudeChatAccent::default(), true, &warp);

    assert!(accent.b > accent.r, "{accent:?} isn't blue");
    assert_ne!(Palette::dark().accent, Hex::rgb(0xd97757));
    assert_ne!(Palette::light().accent, Hex::rgb(0xc96442));
}

#[test]
fn each_accent_choice_maps_to_its_own_color() {
    let warp = theme(0x101820, 0xf0f0f0, 0x123456);
    for dark in [true, false] {
        let colors: Vec<_> = ClaudeChatAccent::ALL
            .into_iter()
            .map(|choice| accent_color(choice, dark, &warp))
            .collect();
        for (index, color) in colors.iter().enumerate() {
            assert!(!colors[index + 1..].contains(color), "{color:?} repeats");
        }
    }
    assert_eq!(
        accent_color(ClaudeChatAccent::Theme, true, &warp),
        Hex::rgb(0x123456).0
    );
}

#[test]
fn the_accent_sets_its_text_and_selection() {
    let dark = Palette::dark().with_accent(Hex::rgb(0x112233).0);
    assert_eq!(dark.accent, Hex::rgb(0x112233));
    assert_eq!(dark.on_accent, Hex::rgb(0xffffff));
    assert_eq!(dark.selection.0.r, 0x11);
    assert!(dark.selection.0.a < 0xff);

    let pale = Palette::light().with_accent(Hex::rgb(0xffe08a).0);
    assert_ne!(pale.on_accent, Hex::rgb(0xffffff));
}

#[test]
fn file_overrides_apply_on_top_of_the_warp_palette() {
    let file = DesignFile::from_toml("[dark]\nborder = \"#010203\"").unwrap();
    let warp = theme(0x101820, 0xf0f0f0, 0x19aad8);
    let palettes = ResolvedPalettes::resolve(&file, &warp, ClaudeChatAccent::Purple);

    assert_eq!(palettes.warp.border, Hex::rgb(0x010203));
    assert_eq!(palettes.claude_dark.border, Hex::rgb(0x010203));
    assert_eq!(palettes.claude_light.border, Palette::light().border);
    assert_eq!(
        palettes.warp.accent.0,
        accent_color(ClaudeChatAccent::Purple, true, &warp)
    );
    assert_eq!(palettes.warp.page, Hex::rgb(0x101820));
}

#[test]
fn the_claude_theme_follows_warp_by_default() {
    assert_eq!(ClaudeTheme::default(), ClaudeTheme::Warp);
}

#[test]
fn the_font_size_choice_maps_to_points() {
    assert_eq!(chat_font_size(ClaudeChatFontSize::FollowWarp, 13.), 13.);
    assert_eq!(chat_font_size(ClaudeChatFontSize::FollowWarp, 40.), 24.);
    assert_eq!(chat_font_size(ClaudeChatFontSize::Size15, 13.), 15.);
    assert_eq!(chat_font_size(ClaudeChatFontSize::Size18, 13.), 18.);
    assert_eq!(
        ClaudeChatFontSize::default(),
        ClaudeChatFontSize::FollowWarp
    );
    let fixed: Vec<_> = ClaudeChatFontSize::ALL
        .into_iter()
        .filter_map(ClaudeChatFontSize::points)
        .collect();
    assert!(fixed.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn sizes_scale_with_the_font_size() {
    let base = Sizes::default();
    let bigger = base.clone().scaled_to(base.font_size * 2.);

    assert_eq!(bigger.font_size, base.font_size * 2.);
    assert_eq!(bigger.code_font_size, base.code_font_size * 2.);
    assert_eq!(bigger.small_font_size, base.small_font_size * 2.);
    assert_eq!(bigger.tool_row_height, base.tool_row_height * 2.);
    assert_eq!(bigger.column_max_width, base.column_max_width);
    assert_eq!(bigger.card_radius, base.card_radius);
    assert_eq!(base.clone().scaled_to(base.font_size), base);
}

#[test]
fn file_sizes_win_over_the_scaled_ones() {
    let file = DesignFile::from_toml("[sizes]\ncode_font_size = 20.0").unwrap();
    let sizes = file.sizes_at(13.);

    assert_eq!(sizes.font_size, 13.);
    assert_eq!(sizes.code_font_size, 20.);
    assert!(sizes.small_font_size < Sizes::default().small_font_size);
    assert_eq!(
        DesignFile::default().sizes_at(13.),
        Sizes::default().scaled_to(13.)
    );
}
