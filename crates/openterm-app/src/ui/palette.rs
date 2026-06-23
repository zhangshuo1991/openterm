//! Command palette overlay (Cmd/Ctrl+K). A single fuzzy-searchable list of the
//! actions available right now — the home for every secondary capability, so
//! the main UI needs no permanent buttons. Arrow keys navigate, Enter runs,
//! Esc closes; rows are also clickable.

use iced::widget::{button, column, container, row, text, text_input, Space};
use iced::{Border, Color, Element, Length};

use crate::message::Message;
use crate::theme;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let input = text_input("Type a command…", &app.palette_query)
        .on_input(Message::PaletteQueryChanged)
        .on_submit(Message::PaletteRunSelected)
        .padding([11, 14])
        .size(15)
        .style(|_, _| text_input::Style {
            background: theme::surface_2().into(),
            border: Border {
                color: theme::accent(),
                width: 1.0,
                radius: 9.0.into(),
            },
            icon: theme::text_muted(),
            placeholder: theme::text_dim(),
            value: theme::text_high(),
            selection: theme::accent_soft(),
        });

    let actions = crate::palette::actions_for(app, &app.palette_query);

    let mut list = column![].spacing(2);
    if actions.is_empty() {
        list = list.push(
            container(text("No matching commands").size(13).color(theme::text_dim()))
                .padding([10, 12]),
        );
    } else {
        for (index, action) in actions.iter().enumerate() {
            let selected = index == app.palette_selected;
            list = list.push(action_row(
                action.title,
                action.hint,
                selected,
                Message::PaletteRun(Box::new(action.message.clone())),
            ));
        }
    }

    let body = column![input, Space::new().height(Length::Fixed(10.0)), list].spacing(2);

    let card = container(body)
        .padding(14)
        .width(Length::Fixed(560.0))
        .style(|_| container::Style {
            background: Some(theme::surface_1().into()),
            border: Border {
                color: theme::border_strong(),
                width: 1.0,
                radius: 13.0.into(),
            },
            ..Default::default()
        });

    // Click outside the card closes the palette.
    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::ClosePalette)
        .style(|_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.45).into()),
            ..Default::default()
        });

    iced::widget::stack![
        backdrop,
        container(
            column![Space::new().height(Length::Fixed(86.0)), card]
                .align_x(iced::Alignment::Center)
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill),
    ]
    .into()
}

fn action_row<'a>(
    title: &'a str,
    hint: &'a str,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message> {
    let body = row![
        text(title)
            .size(14)
            .color(theme::text_high())
            .width(Length::FillPortion(2)),
        text(hint)
            .size(12)
            .color(theme::text_muted())
            .width(Length::FillPortion(3)),
    ]
    .spacing(12)
    .align_y(iced::Alignment::Center);

    button(body)
        .width(Length::Fill)
        .padding([9, 12])
        .on_press(on_press)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if selected {
                theme::accent_soft()
            } else if hovered {
                theme::surface_2()
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(bg.into()),
                text_color: theme::text_high(),
                border: Border {
                    color: if selected {
                        theme::with_alpha(theme::accent(), 0.5)
                    } else {
                        Color::TRANSPARENT
                    },
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}
