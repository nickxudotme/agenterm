use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::Vector2F;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::{Fill, WarpTheme};
use warpui::elements::{
    Align, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Flex, FormattedTextElement,
    HighlightedHyperlink, Icon, MouseStateHandle, ParentElement, Radius, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::ui_components::components::{UiComponent as _, UiComponentStyles};
use warpui::{AppContext, Element, EventContext, PaintContext, SingletonEntity as _};

use super::settings::WarpifySettings;
use crate::ai::blocklist::inline_action::inline_action_icons;
use crate::ui_components::blended_colors;

pub const HORIZONTAL_TEXT_MARGIN: f32 = 20.;
pub const SSH_DOCS_URL: &str = "https://docs.warp.dev/terminal/warpify/ssh";
pub const SUBSHELL_DOCS_URL: &str = "https://docs.warp.dev/terminal/warpify/subshells";

/// Errored blocks have a red stripe, and subshells have a gray one.
pub const LEFT_STRIPE_WIDTH: f32 = 5.;

pub fn build_header_row(
    text: &'static str,
    icon: Icon,
    theme: &WarpTheme,
    appearance: &Appearance,
) -> Container {
    let mut row = Flex::row();
    row.add_child(
        ConstrainedBox::new(icon.finish())
            .with_height(appearance.monospace_font_size() + 2.)
            .with_width(appearance.monospace_font_size() + 2.)
            .finish(),
    );

    row.add_child(
        Container::new(
            Text::new(
                text,
                appearance.monospace_font_family(),
                appearance.monospace_font_size(),
            )
            .with_style(Properties::default().weight(Weight::Bold))
            .with_color(theme.active_ui_text_color().into())
            .finish(),
        )
        .with_margin_left(8.)
        .finish(),
    );

    Container::new(row.finish())
}

pub fn apply_spacing_styles(header_row: Container) -> Container {
    header_row
        .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
        .with_margin_top(8.)
}

/// UI helper to render the header of an SSH rich content block.
pub fn header_row(
    text: &'static str,
    icon: Icon,
    theme: &WarpTheme,
    appearance: &Appearance,
) -> Box<dyn Element> {
    apply_spacing_styles(build_header_row(text, icon, theme, appearance)).finish()
}

fn green_check_icon(appearance: &Appearance, size: f32) -> Box<dyn Element> {
    ConstrainedBox::new(inline_action_icons::green_check_icon(appearance).finish())
        .with_max_height(size)
        .with_max_width(size)
        .finish()
}

/// UI helper to render the ssh command that caused the warpification prompt.
pub fn build_command_row(
    command: String,
    theme: &WarpTheme,
    appearance: &Appearance,
    show_green_check: bool,
) -> Container {
    let text = FormattedTextElement::from_str(
        command,
        appearance.monospace_font_family(),
        appearance.monospace_font_size(),
    )
    .with_color(blended_colors::text_main(theme, theme.background()))
    .finish();

    let icon_size = appearance.monospace_font_size() + 2.;
    let icon = Container::new(green_check_icon(appearance, icon_size))
        .with_margin_right(icon_size)
        .finish();

    let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
    if show_green_check {
        row.add_child(icon);
    }
    row.add_child(Shrinkable::new(1., text).finish());

    Container::new(row.finish())
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .with_background_color(theme.background().into_solid())
        .with_vertical_padding(12.)
        .with_horizontal_padding(16.)
        .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
        .with_margin_top(16.)
}

/// UI helper to render the description row of an SSH rich content block.
pub fn build_description_row(
    text: FormattedText,
    theme: &WarpTheme,
    appearance: &Appearance,
    highlight_index: HighlightedHyperlink,
) -> FormattedTextElement {
    let font_size = appearance.monospace_font_size();
    let font_family = appearance.monospace_font_family();
    let code_font_family = appearance.monospace_font_family();
    let font_color = blended_colors::text_sub(theme, theme.background());

    FormattedTextElement::new(
        text,
        font_size,
        font_family,
        code_font_family,
        font_color,
        highlight_index.clone(),
    )
}

pub fn description_row(text: &str, theme: &WarpTheme, appearance: &Appearance) -> Box<dyn Element> {
    let text = FormattedText::new(vec![FormattedTextLine::Line(vec![
        FormattedTextFragment::plain_text(text),
    ])]);

    apply_spacing_styles(Container::new(
        build_description_row(text, theme, appearance, Default::default()).finish(),
    ))
    .finish()
}

/// Renders a "Never Warpify this host" link or nothing.
pub fn render_never_warpify_ssh_link(
    ssh_host: &Option<String>,
    app: &AppContext,
    appearance: &Appearance,
    mouse_state_handle: MouseStateHandle,
    on_never_warpify: fn(&mut EventContext<'_>, ssh_host: String),
) -> Option<Box<dyn Element>> {
    let Some(ssh_host) = ssh_host else {
        return None;
    };

    let settings = WarpifySettings::handle(app);
    if settings.as_ref(app).is_ssh_host_denylisted(ssh_host) {
        // Should only happen if user manually attempts to Warpify a denylisted host.
        return None;
    }

    let link = appearance
        .ui_builder()
        .link(
            "Never Warpify this host".into(),
            None,
            Some(Box::new({
                let ssh_host = ssh_host.clone();
                move |ctx| on_never_warpify(ctx, ssh_host.to_owned())
            })),
            mouse_state_handle,
        )
        .soft_wrap(false)
        .with_style(UiComponentStyles {
            font_size: Some(appearance.monospace_font_size()),
            font_family_id: Some(appearance.monospace_font_family()),
            ..Default::default()
        })
        .build()
        .finish();

    Some(Align::new(link).bottom_right().finish())
}

/// A single solid color vertical bar positioned on the left-hand side of a blocklist element
/// used to indicate contextual state such as failures or AI context.
pub fn draw_flag_pole(
    origin: Vector2F,
    height: f32,
    fill: impl Into<Fill>,
    ctx: &mut PaintContext,
) {
    ctx.scene
        .draw_rect_with_hit_recording(RectF::new(origin, Vector2F::new(LEFT_STRIPE_WIDTH, height)))
        .with_background(fill.into());
}
