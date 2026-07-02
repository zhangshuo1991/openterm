//! SFTP dual-pane file manager. Local files on the left, remote on the right.
//! Transfer actions live in each pane's toolbar (Upload on local, Download on
//! remote) — there is no center column. Columns are sortable, and a transfers
//! panel along the bottom shows live progress + speed and recent history.
//!
//! Every remote operation runs over the session's *live* SSH connection.

use iced::widget::{
    button, column, container, mouse_area, progress_bar, row, scrollable, text, text_input, Space,
};
use iced::{Border, Color, Element, Length};

use crate::connection::Direction;
use crate::message::Message;
use crate::session::{ChmodState, Session, SftpSide, SortField, Transfer, TransferStatus};
use crate::theme;
use crate::App;

pub fn view(app: &App) -> Element<'_, Message> {
    let Some(session) = app.active_session() else {
        return container(Space::new()).into();
    };

    // When the file viewer is open, hide the local pane and show Remote + Viewer.
    let panes: Element<'_, Message> = if let Some(fv) = &session.file_viewer {
        row![
            container(remote_pane(app, session)).width(Length::FillPortion(2)),
            container(super::file_viewer::view(fv)).width(Length::FillPortion(3)),
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    } else {
        row![
            container(local_pane(app, session)).width(Length::FillPortion(1)),
            container(remote_pane(app, session)).width(Length::FillPortion(1)),
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    };

    let mut root = column![panes].spacing(10);
    if !session.transfers.is_empty() {
        root = root.push(transfers_panel(session));
    } else {
        root = root.push(
            text(session.sftp_status.clone())
                .size(12)
                .color(theme::text_muted()),
        );
    }

    let base = container(root.padding(14).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(theme::surface_0().into()),
            ..Default::default()
        });

    // Chmod modal overlay when active.
    if let Some(chmod) = &session.sftp_chmod {
        iced::widget::stack![base, chmod_modal(chmod)].into()
    } else {
        base.into()
    }
}

// --- panes ---

fn local_pane<'a>(app: &'a App, session: &'a Session) -> Element<'a, Message> {
    let toolbar = row![
        nav_button("Up", Message::SftpLocalParentDir),
        container(path_field(
            &session.local_path,
            Message::SftpLocalPathChanged,
            Message::SftpRefresh
        ))
        .width(Length::Fill),
        nav_button("New folder", Message::SftpStartNewFolder(SftpSide::Local)),
        nav_button("Refresh", Message::SftpRefresh),
        nav_button("Upload", Message::SftpUploadSelected),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let mut list = column![].spacing(1);
    if let Some(err) = &session.local_error {
        list = list.push(
            text(err.clone()).size(11).color(iced::Color::from_rgb(0.9, 0.3, 0.3)),
        );
    }
    for (index, entry) in session.local_files.iter().enumerate() {
        let selected = session.selected_local.contains(&index);
        // Single click selects; a double click on a folder enters it (the
        // double-click is detected in `update`, so every row sends Select).
        let on_press = Message::SftpSelectLocal(index);
        let menu_open = app.sftp_menu == Some((SftpSide::Local, index));
        list = list.push(file_row(
            FileRow {
                side: SftpSide::Local,
                index,
                is_dir: entry.is_dir,
                name: &entry.name,
                size: human_size(entry.size),
                modified: None,
                perms: None,
                selected,
                menu_open,
            },
            on_press,
        ));
    }

    pane("Local", SftpSide::Local, app, toolbar, list)
}

fn remote_pane<'a>(app: &'a App, session: &'a Session) -> Element<'a, Message> {
    let toolbar = row![
        nav_button("Up", Message::SftpParentDir),
        container(path_field(
            &session.remote_path,
            Message::SftpRemotePathChanged,
            Message::SftpRefresh
        ))
        .width(Length::Fill),
        nav_button("New folder", Message::SftpStartNewFolder(SftpSide::Remote)),
        nav_button("Refresh", Message::SftpRefresh),
        nav_button("Download", Message::SftpDownloadSelected),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let mut list = column![].spacing(1);
    for (index, entry) in session.remote_files.iter().enumerate() {
        let is_dir = matches!(entry.kind, openterm_ssh::RemoteFileKind::Directory);
        let selected = session.selected_remote.contains(&index);
        let on_press = Message::SftpSelectRemote(index);
        let menu_open = app.sftp_menu == Some((SftpSide::Remote, index));
        list = list.push(file_row(
            FileRow {
                side: SftpSide::Remote,
                index,
                is_dir,
                name: &entry.name,
                size: human_size(entry.size.unwrap_or(0)),
                modified: entry.modified,
                perms: entry.permissions.map(fmt_mode),
                selected,
                menu_open,
            },
            on_press,
        ));
    }

    pane("Remote", SftpSide::Remote, app, toolbar, list)
}

fn pane<'a>(
    title: &'a str,
    side: SftpSide,
    app: &'a App,
    toolbar: iced::widget::Row<'a, Message>,
    list: iced::widget::Column<'a, Message>,
) -> Element<'a, Message> {
    let header = row![
        text(title)
            .size(14)
            .color(theme::text_high())
            .width(Length::Fill),
        sort_button("Name", SortField::Name, side, app),
        sort_button("Size", SortField::Size, side, app),
        sort_button("Date", SortField::Modified, side, app),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    container(
        column![header, toolbar, scrollable(list.padding(iced::Padding { right: 14.0, ..Default::default() })).height(Length::Fill)]
            .spacing(10)
            .padding(12)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_subtle(),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    })
    .into()
}

// --- shared widgets ---

fn sort_button<'a>(label: &'a str, field: SortField, side: SftpSide, app: &'a App) -> Element<'a, Message> {
    let (cur_sort, cur_asc) = match side {
        SftpSide::Local => (app.sftp_sort_local, app.sftp_sort_asc_local),
        SftpSide::Remote => (app.sftp_sort, app.sftp_sort_asc),
    };
    let active = cur_sort == field;
    let caption = if active {
        format!("{label} {}", if cur_asc { "▲" } else { "▼" })
    } else {
        label.to_string()
    };
    button(text(caption).size(11).color(if active {
        theme::accent_strong()
    } else {
        theme::text_muted()
    }))
    .padding([4, 8])
    .on_press(Message::SftpSetSort(side, field))
    .style(move |_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(
                if active {
                    theme::accent_soft()
                } else if hovered {
                    theme::surface_3()
                } else {
                    Color::TRANSPARENT
                }
                .into(),
            ),
            text_color: theme::text_high(),
            border: Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

fn path_field<'a>(
    value: &str,
    on_input: impl Fn(String) -> Message + 'a,
    on_submit: Message,
) -> Element<'a, Message> {
    text_input("path", value)
        .on_input(on_input)
        .on_submit(on_submit)
        .padding([7, 10])
        .size(13)
        .style(|_, status| {
            let focused = matches!(status, text_input::Status::Focused { .. });
            text_input::Style {
                background: theme::surface_2().into(),
                border: Border {
                    color: if focused {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    },
                    width: 1.0,
                    radius: 7.0.into(),
                },
                icon: theme::text_muted(),
                placeholder: theme::text_dim(),
                value: theme::text_high(),
                selection: theme::accent_soft(),
            }
        })
        .into()
}

/// A small button used inside a transfer row. `danger` tints it red (Cancel).
fn ctrl_button(label: &str, on_press: Message, danger: bool) -> Element<'_, Message> {
    button(text(label.to_string()).size(10).color(if danger {
        theme::status_error()
    } else {
        theme::text_high()
    }))
    .padding([3, 8])
    .on_press(on_press)
    .style(move |_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(if hovered { theme::surface_3() } else { theme::surface_2() }.into()),
            text_color: if danger { theme::status_error() } else { theme::text_high() },
            border: Border {
                color: theme::border_subtle(),
                width: 1.0,
                radius: 5.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn nav_button(label: &str, on_press: Message) -> Element<'_, Message> {
    let primary = label == "Upload" || label == "Download";
    button(text(label.to_string()).size(12).color(if primary {
        theme::surface_0()
    } else {
        theme::text_high()
    }))
    .padding([6, 11])
    .on_press(on_press)
    .style(move |_, status| {
        let hovered = matches!(status, button::Status::Hovered);
        let bg = if primary {
            if hovered {
                theme::accent_strong()
            } else {
                theme::accent()
            }
        } else if hovered {
            theme::surface_3()
        } else {
            theme::surface_2()
        };
        button::Style {
            background: Some(bg.into()),
            text_color: if primary {
                theme::surface_0()
            } else {
                theme::text_high()
            },
            border: Border {
                color: if primary {
                    Color::TRANSPARENT
                } else {
                    theme::border_subtle()
                },
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

struct FileRow<'a> {
    side: SftpSide,
    index: usize,
    is_dir: bool,
    name: &'a str,
    size: String,
    /// Unix mtime — only for remote rows.
    modified: Option<u32>,
    /// Formatted permission string — only for remote rows.
    perms: Option<String>,
    selected: bool,
    menu_open: bool,
}

fn file_row<'a>(r: FileRow<'a>, on_press: Message) -> Element<'a, Message> {
    let tag = text(if r.is_dir { "DIR" } else { "" })
        .font(theme::TERMINAL_FONT)
        .size(10)
        .color(theme::accent_strong())
        .width(Length::Fixed(30.0));
    let name_color = if r.is_dir { theme::text_high() } else { theme::text_muted() };
    let selected = r.selected;

    let meta_col = |s: String| {
        text(s)
            .font(theme::TERMINAL_FONT)
            .size(10)
            .color(theme::text_dim())
    };

    let mut label = row![
        tag,
        text(r.name.to_string()).size(13).color(name_color).width(Length::Fill),
        // Size column
        container(meta_col(if r.is_dir { String::new() } else { r.size }))
            .width(Length::Fixed(60.0))
            .align_x(iced::alignment::Horizontal::Right),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    if r.modified.is_some() || r.perms.is_some() {
        label = label.push(
            container(meta_col(r.modified.map(fmt_mtime).unwrap_or_default()))
                .width(Length::Fixed(82.0))
                .align_x(iced::alignment::Horizontal::Right),
        );
        label = label.push(
            container(meta_col(r.perms.unwrap_or_default()))
                .width(Length::Fixed(72.0))
                .align_x(iced::alignment::Horizontal::Right),
        );
    }

    let clickable = mouse_area(
        button(label)
            .width(Length::Fill)
            .padding([6, 9])
            .on_press(on_press)
            .style(move |_, status| {
                let hovered = matches!(status, button::Status::Hovered);
                let bg = if selected { theme::accent_soft() }
                    else if hovered { theme::surface_2() }
                    else { Color::TRANSPARENT };
                button::Style {
                    background: Some(bg.into()),
                    text_color: theme::text_high(),
                    border: Border { radius: 6.0.into(), ..Default::default() },
                    ..Default::default()
                }
            }),
    )
    .on_right_press(Message::SftpOpenMenu(r.side, r.index));

    if r.menu_open {
        column![clickable, context_menu(r.side)].spacing(2).into()
    } else {
        clickable.into()
    }
}

/// The inline action strip shown under a right-clicked row.
fn context_menu(side: SftpSide) -> Element<'static, Message> {
    let mut actions = row![].spacing(4);
    let (label, msg) = match side {
        SftpSide::Local => ("Upload", Message::SftpMenuUpload),
        SftpSide::Remote => ("Download", Message::SftpMenuDownload),
    };
    actions = actions.push(menu_button(label, msg, false));
    actions = actions.push(menu_button("Rename", Message::SftpStartRename, false));
    if matches!(side, SftpSide::Remote) {
        actions = actions.push(menu_button("Chmod", Message::SftpMenuChmod, false));
    }
    actions = actions
        .push(menu_button("Delete", Message::SftpMenuDelete, true))
        .push(menu_button("Close", Message::SftpCloseMenu, false));

    container(actions)
        .padding([5, 8])
        .style(|_| container::Style {
            background: Some(theme::surface_2().into()),
            border: Border {
                color: theme::border_strong(),
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn menu_button(label: &str, msg: Message, danger: bool) -> Element<'static, Message> {
    let fg = if danger {
        theme::status_error()
    } else {
        theme::text_high()
    };
    button(text(label.to_string()).size(11).color(fg))
        .padding([4, 9])
        .on_press(msg)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if hovered {
                if danger {
                    theme::with_alpha(theme::status_error(), 0.2)
                } else {
                    theme::surface_3()
                }
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: Some(bg.into()),
                text_color: fg,
                border: Border {
                    radius: 5.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}

// --- transfers panel ---

fn transfers_panel(session: &Session) -> Element<'_, Message> {
    let mut list = column![].spacing(4);
    for t in &session.transfers {
        list = list.push(transfer_row(t));
    }
    container(
        column![
            text("Transfers").size(13).color(theme::text_high()),
            scrollable(list).height(Length::Fixed(118.0)),
        ]
        .spacing(8)
        .padding(12),
    )
    .width(Length::Fill)
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_subtle(),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn transfer_row(t: &Transfer) -> Element<'_, Message> {
    let dir = match t.direction {
        Direction::Upload => "UP",
        Direction::Download => "DN",
    };
    let dir_color = match t.direction {
        Direction::Upload => theme::status_warn(),
        Direction::Download => theme::accent_strong(),
    };

    let right: Element<'_, Message> = match &t.status {
        TransferStatus::Active => {
            let known = t.total > 0;
            let frac = if known {
                (t.transferred as f32 / t.total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let pct_label = if known {
                format!("{:.0}%", frac * 100.0)
            } else {
                "…".to_string()
            };
            // size readout: "1.2M / 4.5M" (or just transferred when total unknown)
            let size_label = if known {
                format!("{} / {}", human_size(t.transferred), human_size(t.total))
            } else {
                human_size(t.transferred)
            };

            let bar = container(
                progress_bar(0.0..=1.0, frac)
                    .girth(Length::Fixed(8.0))
                    .style(|_| progress_bar::Style {
                        background: theme::surface_3().into(),
                        bar: theme::accent_strong().into(),
                        border: Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                    }),
            )
            .width(Length::Fixed(160.0));

            // Time readout: elapsed always; ETA when it can be estimated.
            // e.g. "0:12 · ETA 0:47" or just "0:12" before the first estimate.
            let time_label = match t.eta_secs() {
                Some(eta) => format!(
                    "{} · ETA {}",
                    fmt_duration(t.elapsed().as_secs_f64()),
                    fmt_duration(eta)
                ),
                None => fmt_duration(t.elapsed().as_secs_f64()),
            };

            // While a pause is pending (worker still draining), show "Pausing…"
            // and disable the Pause button so it reads as acknowledged.
            let pause_ctrl: Element<'_, Message> = if t.pause_requested {
                text("Pausing…")
                    .font(theme::TERMINAL_FONT)
                    .size(10)
                    .color(theme::status_warn())
                    .into()
            } else {
                ctrl_button("Pause", Message::TransferPause(t.id), false)
            };

            row![
                text(pct_label)
                    .font(theme::TERMINAL_FONT)
                    .size(11)
                    .color(theme::text_high())
                    .width(Length::Fixed(36.0)),
                bar,
                text(size_label)
                    .font(theme::TERMINAL_FONT)
                    .size(11)
                    .color(theme::text_muted())
                    .width(Length::Fixed(120.0)),
                text(speed_label(t.speed_bps))
                    .font(theme::TERMINAL_FONT)
                    .size(11)
                    .color(theme::accent())
                    .width(Length::Fixed(80.0)),
                text(time_label)
                    .font(theme::TERMINAL_FONT)
                    .size(11)
                    .color(theme::text_muted())
                    .width(Length::Fixed(130.0)),
                pause_ctrl,
                ctrl_button("Cancel", Message::TransferCancel(t.id), true),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        }
        TransferStatus::Paused => row![
            text("Paused")
                .font(theme::TERMINAL_FONT)
                .size(11)
                .color(theme::status_warn())
                .width(Length::Fixed(52.0)),
            text(format!("{} / {}", human_size(t.transferred), human_size(t.total)))
                .font(theme::TERMINAL_FONT)
                .size(11)
                .color(theme::text_muted())
                .width(Length::Fixed(120.0)),
            ctrl_button("Resume", Message::TransferResume(t.id), false),
            ctrl_button("Cancel", Message::TransferCancel(t.id), true),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into(),
        TransferStatus::Done => text(format!(
            "Done · {} · {}",
            human_size(t.total),
            fmt_duration(t.elapsed().as_secs_f64())
        ))
        .size(11)
        .color(theme::status_ok())
        .into(),
        TransferStatus::Failed(e) => text(format!("Failed: {e}"))
            .size(11)
            .color(theme::status_error())
            .into(),
    };

    row![
        text(dir)
            .font(theme::TERMINAL_FONT)
            .size(10)
            .color(dir_color)
            .width(Length::Fixed(26.0)),
        text(t.name.clone())
            .size(12)
            .color(theme::text_high())
            .width(Length::Fill),
        right,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}

fn speed_label(bps: f64) -> String {
    if bps <= 0.0 {
        return String::new();
    }
    format!("{}/s", human_size(bps as u64))
}

/// Format a duration in seconds as a compact clock: "0:07", "1:23", or
/// "1:02:33" once it passes an hour.
fn fmt_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

// --- name prompt overlay (new folder / rename) ---

/// Modal overlay for entering a folder name or a new name (rename). Returns an
/// empty space if no prompt is active.
pub fn prompt_overlay(app: &App) -> Element<'_, Message> {
    use crate::session::SftpPromptKind;
    let Some(prompt) = &app.sftp_prompt else {
        return container(Space::new()).into();
    };

    let (title, hint) = match &prompt.kind {
        SftpPromptKind::NewFolder => ("New folder", "Folder name"),
        SftpPromptKind::Rename { old, .. } => ("Rename", old.as_str()),
    };

    let input = text_input(hint, &prompt.value)
        .on_input(Message::SftpPromptChanged)
        .on_submit(Message::SftpPromptConfirm)
        .padding([9, 12])
        .size(14)
        .style(|_, status| {
            let focused = matches!(status, text_input::Status::Focused { .. });
            text_input::Style {
                background: theme::surface_2().into(),
                border: Border {
                    color: if focused {
                        theme::accent()
                    } else {
                        theme::border_subtle()
                    },
                    width: 1.0,
                    radius: 7.0.into(),
                },
                icon: theme::text_muted(),
                placeholder: theme::text_dim(),
                value: theme::text_high(),
                selection: theme::accent_soft(),
            }
        });

    let buttons = row![
        Space::new().width(Length::Fill),
        nav_button("Cancel", Message::SftpPromptCancel),
        nav_button("Confirm", Message::SftpPromptConfirm),
    ]
    .spacing(8);

    let card = container(
        column![text(title).size(16).color(theme::text_high()), input, buttons,].spacing(12),
    )
    .padding(18)
    .width(Length::Fixed(360.0))
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_strong(),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    });

    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::SftpPromptCancel)
        .style(|_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.5).into()),
            ..Default::default()
        });

    iced::widget::stack![
        backdrop,
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    ]
    .into()
}

// --- delete confirmation overlay ---

/// Modal asking the user to confirm a delete. Returns empty space when no
/// delete is pending.
pub fn confirm_delete_overlay(app: &App) -> Element<'_, Message> {
    let Some(confirm) = app.sftp_confirm() else {
        return container(Space::new()).into();
    };

    let where_ = match confirm.side {
        SftpSide::Local => "local",
        SftpSide::Remote => "remote",
    };
    let title = if confirm.count > 1 {
        format!("Delete {} items?", confirm.count)
    } else if confirm.any_dir {
        "Delete folder?".to_string()
    } else {
        "Delete file?".to_string()
    };
    let warning = if confirm.any_dir {
        "Folders are deleted with everything inside them. This cannot be undone."
    } else {
        "This cannot be undone."
    };

    let buttons = row![
        Space::new().width(Length::Fill),
        nav_button("Cancel", Message::SftpCancelDelete),
        danger_button("Delete", Message::SftpConfirmDelete),
    ]
    .spacing(8);

    let card = container(
        column![
            text(title).size(16).color(theme::text_high()),
            text(format!("{} · {}", where_, confirm.label))
                .size(13)
                .color(theme::text_high()),
            text(warning).size(12).color(theme::text_muted()),
            buttons,
        ]
        .spacing(12),
    )
    .padding(18)
    .width(Length::Fixed(380.0))
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_strong(),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    });

    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::SftpCancelDelete)
        .style(|_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.5).into()),
            ..Default::default()
        });

    iced::widget::stack![
        backdrop,
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    ]
    .into()
}

/// A destructive (red) action button for confirmation dialogs.
fn danger_button(label: &str, on_press: Message) -> Element<'_, Message> {
    button(text(label.to_string()).size(12).color(Color::WHITE))
        .padding([6, 11])
        .on_press(on_press)
        .style(|_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(
                    if hovered {
                        Color::from_rgb(0.78, 0.22, 0.22)
                    } else {
                        Color::from_rgb(0.70, 0.18, 0.18)
                    }
                    .into(),
                ),
                text_color: Color::WHITE,
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 1.0,
                    radius: 7.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

// ── Chmod modal ───────────────────────────────────────────────────────────────

/// Centered modal for changing file permissions.
fn chmod_modal(chmod: &ChmodState) -> Element<'_, Message> {
    let symbolic = fmt_mode(chmod.current_mode);
    let card = container(
        column![
            text("Change permissions").size(16).color(theme::text_high()),
            text(chmod.path.clone()).size(11).color(theme::text_muted()),
            Space::new().height(Length::Fixed(4.0)),
            // Symbolic display of current mode
            text(format!("Current: {symbolic} ({:o})", chmod.current_mode))
                .font(theme::TERMINAL_FONT)
                .size(12)
                .color(theme::text_high()),
            Space::new().height(Length::Fixed(8.0)),
            column![
                text("New mode (octal)").size(12).color(theme::text_muted()),
                text_input("755", &chmod.input)
                    .on_input(Message::SftpChmodInput)
                    .padding([8, 10])
                    .size(16)
                    .style(|_, status| {
                        let focused = matches!(status, text_input::Status::Focused { .. });
                        text_input::Style {
                            background: theme::surface_2().into(),
                            border: Border {
                                color: if focused { theme::accent() } else { theme::border_subtle() },
                                width: 1.0,
                                radius: 7.0.into(),
                            },
                            icon: theme::text_muted(),
                            placeholder: theme::text_dim(),
                            value: theme::text_high(),
                            selection: theme::accent_soft(),
                        }
                    })
                    .width(Length::Fixed(120.0)),
                // Live symbolic preview of the typed mode
                {
                    let preview = u32::from_str_radix(&chmod.input, 8)
                        .map(fmt_mode)
                        .unwrap_or_else(|_| "invalid".to_string());
                    text(preview)
                        .font(theme::TERMINAL_FONT)
                        .size(12)
                        .color(theme::accent_strong())
                },
            ].spacing(5),
            Space::new().height(Length::Fixed(12.0)),
            row![
                Space::new().width(Length::Fill),
                action_btn("Cancel", false, Message::SftpChmodCancel),
                action_btn("Apply", true, Message::SftpChmodConfirm),
            ].spacing(8),
        ]
        .spacing(8),
    )
    .padding(22)
    .width(Length::Fixed(340.0))
    .style(|_| container::Style {
        background: Some(theme::surface_1().into()),
        border: Border {
            color: theme::border_strong(),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    });

    let backdrop = button(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::SftpChmodCancel)
        .style(|_, _| button::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.5).into()),
            ..Default::default()
        });

    iced::widget::stack![
        backdrop,
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    ]
    .into()
}

fn action_btn(label: &str, primary: bool, on_press: Message) -> Element<'_, Message> {
    button(text(label.to_string()).size(12))
        .padding([6, 14])
        .on_press(on_press)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered);
            let bg = if primary {
                if hovered { theme::accent_strong() } else { theme::accent() }
            } else if hovered {
                theme::surface_3()
            } else {
                theme::surface_2()
            };
            button::Style {
                background: Some(bg.into()),
                text_color: if primary { theme::surface_0() } else { theme::text_high() },
                border: Border {
                    color: if primary { Color::TRANSPARENT } else { theme::border_subtle() },
                    width: 1.0,
                    radius: 7.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

// ── Formatters ────────────────────────────────────────────────────────────────

/// Format a Unix mode word (lower 12 bits) as `rwxr-xr-x`.
fn fmt_mtime(ts: u32) -> String {
    const MONTHS: [&str; 12] = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    // Convert the SFTP Unix timestamp to local time using the system timezone
    // (respects TZ env var / /etc/localtime), so users in e.g. CST+8 see
    // local time instead of UTC.
    #[cfg(unix)]
    {
        let t = ts as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&t, &mut tm); }
        let mo = (tm.tm_mon as usize).min(11); // 0-based
        return format!(
            "{} {:2} {:02}:{:02}",
            MONTHS[mo],
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
        );
    }
    // Fallback for non-unix builds: keep the old UTC approximation.
    #[cfg(not(unix))]
    {
        let s = ts as u64;
        let h = (s % 86400) / 3600;
        let m = (s % 3600) / 60;
        let days = s / 86400 + 719468;
        let era = days / 146097;
        let doe = days % 146097;
        let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
        let doy = doe - (365*yoe + yoe/4 - yoe/100);
        let mp = (5*doy + 2) / 153;
        let d = doy - (153*mp + 2)/5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        format!("{} {:2} {:02}:{:02}", MONTHS[(mo as usize).saturating_sub(1).min(11)], d, h, m)
    }
}

fn fmt_mode(mode: u32) -> String {
    let bits = mode & 0o7777;
    let mut s = String::with_capacity(10);
    // setuid / setgid / sticky affect the x bit display but keep it simple here.
    for shift in [6u32, 3, 0] {
        let r = (bits >> (shift + 2)) & 1;
        let w = (bits >> (shift + 1)) & 1;
        let x = (bits >> shift) & 1;
        s.push(if r == 1 { 'r' } else { '-' });
        s.push(if w == 1 { 'w' } else { '-' });
        s.push(if x == 1 { 'x' } else { '-' });
    }
    s
}
