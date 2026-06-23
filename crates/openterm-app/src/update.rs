//! Message handling. Translates UI/worker messages into state changes and
//! tasks. Connection lifecycle is the delicate part — see the `Conn` arm.

use iced::{clipboard, Task};
use openterm_terminal::TerminalEngine;

use crate::connection::{Command, ConnectParams, Event as ConnEvent};
use crate::message::Message;
use crate::session::{Phase, SessionConfig};
use crate::{persist_host, App};

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Noop => Task::none(),

        // --- Host sidebar ---
        Message::HostSearchChanged(value) => {
            app.host_search = value;
            Task::none()
        }
        Message::ConnectSavedHost(index) => {
            let Some(host) = app.hosts.get(index).cloned() else {
                return Task::none();
            };
            let config = app.config_from_host(&host);
            connect_in_new_or_active(app, config)
        }
        Message::EditSavedHost(index) => {
            // Load the saved host into the active session's connection form
            // (or a fresh tab if the active one is busy), without connecting.
            let Some(host) = app.hosts.get(index).cloned() else {
                return Task::none();
            };
            let config = app.config_from_host(&host);
            load_config_into_slot(app, config);
            app.status = "Loaded host. Edit and connect when ready.".to_string();
            Task::none()
        }
        Message::DeleteSavedHost(index) => {
            // Ask first — deletion is irreversible.
            if app.hosts.get(index).is_some() {
                app.pending_host_delete = Some(index);
            }
            Task::none()
        }
        Message::ConfirmDeleteHost => {
            if let Some(index) = app.pending_host_delete.take() {
                if let Some(host) = app.hosts.get(index).cloned() {
                    if let Ok(store) = openterm_storage::WorkspaceStore::open(&app.db_path) {
                        let _ = store.delete_host(host.id);
                        app.hosts = store.list_hosts().unwrap_or_default();
                    }
                    app.status = format!("Deleted {}.", host.name);
                }
            }
            Task::none()
        }
        Message::CancelDeleteHost => {
            app.pending_host_delete = None;
            Task::none()
        }
        Message::NewHost => {
            let config = app.blank_config();
            load_config_into_slot(app, config);
            Task::none()
        }

        // --- Tabs ---
        Message::SelectTab(index) => {
            if index < app.sessions.len() {
                app.active = index;
                // Replay the entrance when switching to a tab showing the card.
                if !app.sessions[index].phase.is_active()
                    && !app.sessions[index].terminal_has_content()
                {
                    app.start_card_entrance();
                }
            }
            Task::none()
        }
        Message::StartWindowDrag => {
            // The native titlebar is fused into our chrome, so the empty tab-bar
            // strip stands in for it: a single press drags the window, a double
            // press (≤400ms) zooms it — the macOS title-bar convention.
            let now = std::time::Instant::now();
            let double = app
                .last_title_click
                .is_some_and(|t| now.duration_since(t) <= std::time::Duration::from_millis(400));
            app.last_title_click = if double { None } else { Some(now) };
            if double {
                iced::window::latest().and_then(iced::window::toggle_maximize)
            } else {
                iced::window::latest().and_then(iced::window::drag)
            }
        }
        Message::ModifiersChanged(m) => {
            app.set_modifiers(m);
            Task::none()
        }
        Message::NewTab => {
            let (cols, rows) = app.current_grid();
            let config = app.blank_config();
            app.spawn_session(config, cols, rows);
            app.start_card_entrance();
            Task::none()
        }
        Message::NewLocalShell => {
            let (cols, rows) = app.current_grid();
            app.spawn_local_session(cols, rows);
            // Auto-connect: send a Connect command once the worker hands back its channel.
            // We do this by storing a pending connect with dummy SSH params — local_worker
            // ignores the route and just needs cols/rows via ConnectParams.
            if let Some(session) = app.active_session_mut() {
                session.pending_connect = Some(crate::connection::ConnectParams {
                    route: openterm_ssh::ConnectRoute {
                        target: openterm_core::HostProfile::new("local", "localhost"),
                        target_options: openterm_ssh::ConnectOptions {
                            username: String::new(),
                            auth: openterm_ssh::AuthMethod::AgentOrDefault,
                            trust_unknown_host_keys: false,
                            host_key_policy: openterm_ssh::HostKeyPolicy::TrustAll,
                            timeout: std::time::Duration::from_secs(5),
                        },
                        jump: None,
                    },
                    cols,
                    rows,
                    term: "xterm-256color".to_string(),
                });
            }
            Task::none()
        }
        Message::CloseTab(index) => close_tab(app, index),
        Message::DuplicateTab => {
            let (cols, rows) = app.current_grid();
            if let Some(config) = app.active_session().map(|s| s.config.clone()) {
                app.spawn_session(config, cols, rows);
                return connect_active(app);
            }
            Task::none()
        }

        // --- Connection form ---
        Message::NameChanged(v) => with_config(app, |c| c.name = v),
        Message::HostChanged(v) => with_config(app, |c| c.host = v),
        Message::UserChanged(v) => with_config(app, |c| c.user = v),
        Message::PortChanged(v) => {
            if v.chars().all(|c| c.is_ascii_digit()) && v.len() <= 5 {
                with_config(app, |c| c.port = v)
            } else {
                Task::none()
            }
        }
        Message::AuthModeChanged(mode) => with_config(app, |c| c.auth = mode),
        Message::PasswordChanged(v) => with_config(app, |c| c.password = v),
        Message::KeyPathChanged(v) => with_config(app, |c| c.key_path = v),
        Message::PassphraseChanged(v) => with_config(app, |c| c.passphrase = v),
        Message::GroupChanged(v) => with_config(app, |c| c.group = v),
        Message::TagsChanged(v) => with_config(app, |c| c.tags_str = v),
        Message::BrowseKeyFile => Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .set_title("Select private key file")
                    .pick_file()
                    .await
                    .map(|f| f.path().to_string_lossy().to_string())
            },
            Message::KeyFileSelected,
        ),
        Message::KeyFileSelected(opt) => {
            if let Some(path) = opt {
                with_config(app, |c| c.key_path = path)
            } else {
                Task::none()
            }
        }
        Message::ToggleJump => with_config(app, |c| c.show_jump = !c.show_jump),
        Message::JumpHostChanged(v) => with_config(app, |c| c.jump_host = v),

        Message::Connect => connect_active(app),
        Message::Disconnect => {
            if let Some(session) = app.active_session_mut() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Disconnect);
                }
                session.status = "Disconnecting…".to_string();
            }
            Task::none()
        }
        Message::SaveHost => {
            match app.active_session().map(|s| s.config.clone()) {
                Some(config) => match persist_host(app, &config) {
                    Ok(host_id) => {
                        if let Some(session) = app.active_session_mut() {
                            session.config.host_id = Some(host_id);
                        }
                        if let Ok(store) = openterm_storage::WorkspaceStore::open(&app.db_path) {
                            app.hosts = store.list_hosts().unwrap_or_default();
                        }
                        app.status = "Host saved.".to_string();
                    }
                    Err(error) => app.status = format!("Save failed: {error}"),
                },
                None => {}
            }
            Task::none()
        }

        Message::CloseEditor => Task::none(),

        // --- Host key confirmation ---
        Message::AcceptHostKey => accept_host_key(app),
        Message::RejectHostKey => {
            if let Some(session) = app.active_session_mut() {
                session.host_key = None;
                session.phase = Phase::Failed("Host key rejected.".to_string());
                session.status = "Host key rejected".to_string();
            }
            Task::none()
        }

        // --- Terminal ---
        Message::TerminalInput(bytes) => {
            // Collect a new entry (if any) before any other borrow of `app`.
            let new_entry: Option<openterm_storage::HistoryEntry> = {
                if let Some(session) = app.active_session_mut() {
                    session.terminal.scroll_to_bottom();
                    let prev_len = session.command_history.len();
                    session.track_input(&bytes);
                    if session.command_history.len() > prev_len {
                        session.command_history.last().map(|cmd| openterm_storage::HistoryEntry {
                            ts_ms: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            host: session.config.target_label(),
                            cmd: cmd.clone(),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(entry) = new_entry {
                if let Ok(store) = openterm_storage::WorkspaceStore::open(&app.db_path) {
                    let _ = store.append_history(&entry);
                }
                app.all_history.insert(0, entry);
            }
            if let Some(session) = app.active_session() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Write(bytes));
                }
            }
            Task::none()
        }
        Message::TerminalScroll(lines) => {
            if let Some(session) = app.active_session_mut() {
                // Wheel up (positive) scrolls toward older output.
                let delta = (lines * 3.0).round() as i32;
                if delta != 0 {
                    session.terminal.scroll(delta);
                }
            }
            Task::none()
        }
        Message::WindowResized(size) => {
            app.window_size = size;
            apply_grid(app)
        }
        Message::TerminalAreaResized(_) => apply_grid(app),
        Message::PasteRequested => clipboard::read().map(Message::PasteReady),
        Message::SelectionChanged(sel) => {
            if let Some(session) = app.active_session_mut() {
                session.selection = sel;
            }
            Task::none()
        }
        Message::TerminalCopy => {
            let text = app.active_session().and_then(|s| {
                let (c1, r1, c2, r2) = s.selection?;
                let snap = s.terminal.snapshot();
                let t = crate::terminal_render::selected_text(&snap, (c1, r1), (c2, r2));
                if t.is_empty() { None } else { Some(t) }
            });
            if let Some(t) = text { clipboard::write(t) } else { Task::none() }
        }
        Message::PasteReady(Some(text)) => {
            if let Some(session) = app.active_session() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Write(crate::keys::bracketed_paste(&text)));
                }
            }
            Task::none()
        }
        Message::PasteReady(None) => Task::none(),
        Message::ClearTerminal => {
            if let Some(session) = app.active_session_mut() {
                session.clear_grid();
            }
            Task::none()
        }
        Message::FontSizeDelta(delta) => {
            let next = (app.font_size as i16 + delta).clamp(
                crate::theme::MIN_FONT_SIZE as i16,
                crate::theme::MAX_FONT_SIZE as i16,
            ) as u16;
            if next != app.font_size {
                app.font_size = next;
                app.persist_settings();
                return apply_grid(app);
            }
            Task::none()
        }
        Message::ToggleHistory => {
            app.history_open = !app.history_open;
            // Terminal width changed → resize grid + PTY.
            apply_grid(app)
        }
        Message::HistoryInsert(cmd) => {
            if let Some(session) = app.active_session() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Write(cmd.into_bytes()));
                }
            }
            Task::none()
        }
        Message::HistoryFilterChanged(v) => {
            app.history_filter = v;
            Task::none()
        }
        Message::HistoryCopyCmd(cmd) => clipboard::write(cmd),
        Message::HistoryClearAll => {
            if let Ok(store) = openterm_storage::WorkspaceStore::open(&app.db_path) {
                let _ = store.clear_history();
            }
            app.all_history.clear();
            Task::none()
        }
        Message::HistoryDragStart => {
            app.history_dragging = true;
            Task::none()
        }
        Message::HistoryDragMove(point) => {
            if app.history_dragging {
                // Panel hugs the right edge: width = window_right - cursor_x.
                let width = (app.window_size.width - point.x - crate::ui::HISTORY_DIVIDER_WIDTH)
                    .clamp(200.0, 520.0);
                app.history_width = width;
                // Keep the local grid correct during drag; PTY resize on release.
                let (cols, rows) = app.current_grid();
                if let Some(session) = app.active_session_mut() {
                    session.resize_grid(cols, rows);
                }
            }
            Task::none()
        }
        Message::HistoryDragEnd => {
            app.history_dragging = false;
            apply_grid(app)
        }

        // --- Sidebar resize / collapse / hover ---
        Message::HostHovered(index) => {
            app.hovered_host = index;
            Task::none()
        }
        Message::ToggleSidebar => {
            app.sidebar_collapsed = !app.sidebar_collapsed;
            apply_grid(app)
        }
        Message::SidebarDragStart => {
            app.sidebar_dragging = true;
            Task::none()
        }
        Message::SidebarDragMove(point) => {
            if app.sidebar_dragging {
                // Sidebar hugs the left edge: width = cursor_x.
                app.sidebar_width = point.x.clamp(
                    crate::theme::SIDEBAR_MIN_WIDTH,
                    crate::theme::SIDEBAR_MAX_WIDTH,
                );
                // Keep the local grid correct during drag; PTY resize on release.
                let (cols, rows) = app.current_grid();
                if let Some(session) = app.active_session_mut() {
                    session.resize_grid(cols, rows);
                }
            }
            Task::none()
        }
        Message::SidebarDragEnd => {
            app.sidebar_dragging = false;
            apply_grid(app)
        }

        // --- SFTP ---
        Message::ShowFiles(show) => {
            let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
            app.sftp_menu = None;
            if let Some(session) = app.active_session_mut() {
                session.sftp_open = show;
                if show {
                    session.refresh_local(sort, asc);
                }
            }
            // Terminal width is unaffected; only refresh the remote listing.
            sftp_refresh(app)
        }
        Message::ToggleSftp => {
            let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
            app.sftp_menu = None;
            if let Some(session) = app.active_session_mut() {
                session.sftp_open = !session.sftp_open;
                if session.sftp_open {
                    session.refresh_local(sort, asc);
                }
            }
            sftp_refresh(app)
        }
        Message::ToggleMonitor => {
            // The rail is always-on when connected; this just hides/shows it.
            // Recompute the grid since the terminal width changes.
            app.rail_collapsed = !app.rail_collapsed;
            apply_grid(app)
        }
        Message::MetricsTick => {
            // Sample whenever the active session is connected (the rail is always
            // shown). Process sampling only runs while the expander is open.
            if let Some(session) = app.active_session() {
                if session.phase == Phase::Connected {
                    if let Some(tx) = &session.cmd_tx {
                        let _ = tx.try_send(Command::SampleMetrics);
                        if session.monitor_panel.is_some() {
                            let _ = tx.try_send(Command::SampleProcesses);
                        }
                    }
                }
            }
            Task::none()
        }
        Message::MonitorSelect(panel) => {
            let mut sample = false;
            if let Some(session) = app.active_session_mut() {
                // The rail's process expander: always open on the chosen metric
                // (and re-sort). Collapsing is done via MonitorCloseDetail.
                session.monitor_panel = Some(panel);
                sample = session.phase == Phase::Connected;
            }
            if sample {
                if let Some(session) = app.active_session() {
                    if let Some(tx) = &session.cmd_tx {
                        let _ = tx.try_send(Command::SampleProcesses);
                    }
                }
            }
            Task::none()
        }
        Message::MonitorCloseDetail => {
            if let Some(session) = app.active_session_mut() {
                session.monitor_panel = None;
            }
            Task::none()
        }
        Message::SftpRefresh => {
            refresh_local_active(app);
            sftp_refresh(app)
        }
        Message::SftpSetSort(field) => {
            // Toggle direction if same field, else switch field ascending.
            if app.sftp_sort == field {
                app.sftp_sort_asc = !app.sftp_sort_asc;
            } else {
                app.sftp_sort = field;
                app.sftp_sort_asc = true;
            }
            let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
            if let Some(session) = app.active_session_mut() {
                crate::session::sort_local(&mut session.local_files, sort, asc);
                crate::session::sort_remote(&mut session.remote_files, sort, asc);
                // Indices change under a re-sort; clear selection to avoid
                // pointing at the wrong rows.
                session.selected_local.clear();
                session.selected_remote.clear();
                session.local_anchor = None;
                session.remote_anchor = None;
            }
            Task::none()
        }
        Message::SftpRemotePathChanged(v) => {
            if let Some(session) = app.active_session_mut() {
                session.remote_path = v;
            }
            Task::none()
        }
        Message::SftpSelectRemote(i) => {
            let (toggle, range) = select_mods(app);
            // A plain double click on a folder enters it. A held modifier means
            // the user is extending a selection, so it never navigates.
            let double = register_click(app, crate::session::SftpSide::Remote, i);
            if double && !toggle && !range && is_remote_dir(app, i) {
                return enter_remote(app, i);
            }
            if let Some(session) = app.active_session_mut() {
                session.select_click(crate::session::SftpSide::Remote, i, toggle, range);
            }
            app.sftp_menu = None;
            Task::none()
        }
        Message::SftpEnterRemote(i) => enter_remote(app, i),
        Message::SftpParentDir => {
            app.sftp_menu = None;
            if let Some(session) = app.active_session_mut() {
                session.remote_path = parent_remote(&session.remote_path);
                session.selected_remote.clear();
                session.remote_anchor = None;
            }
            sftp_refresh(app)
        }
        Message::SftpDownloadSelected => {
            start_download(app);
            Task::none()
        }
        Message::SftpDeleteRemoteSelected => {
            delete_remote_selected(app);
            Task::none()
        }
        Message::SftpLocalPathChanged(v) => {
            if let Some(session) = app.active_session_mut() {
                session.local_path = v;
            }
            Task::none()
        }
        Message::SftpSelectLocal(i) => {
            let (toggle, range) = select_mods(app);
            // Same rule as the remote pane: plain double click on a folder enters.
            let double = register_click(app, crate::session::SftpSide::Local, i);
            if double && !toggle && !range && is_local_dir(app, i) {
                return enter_local(app, i);
            }
            if let Some(session) = app.active_session_mut() {
                session.select_click(crate::session::SftpSide::Local, i, toggle, range);
            }
            app.sftp_menu = None;
            Task::none()
        }
        Message::SftpEnterLocal(i) => enter_local(app, i),
        Message::SftpLocalParentDir => {
            app.sftp_menu = None;
            let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
            if let Some(session) = app.active_session_mut() {
                if let Some(parent) = std::path::Path::new(&session.local_path).parent() {
                    session.local_path = parent.display().to_string();
                    session.refresh_local(sort, asc);
                }
            }
            Task::none()
        }
        Message::SftpUploadSelected => {
            start_upload(app);
            Task::none()
        }
        // Context menu + quick operations.
        Message::SftpOpenMenu(side, index) => {
            // Right-click keeps an existing multi-selection if this row is part
            // of it (so the menu acts on the whole set); otherwise it selects
            // just this row.
            if let Some(session) = app.active_session_mut() {
                let set = match side {
                    crate::session::SftpSide::Local => &mut session.selected_local,
                    crate::session::SftpSide::Remote => &mut session.selected_remote,
                };
                if !set.contains(&index) {
                    session.select_click(side, index, false, false);
                }
            }
            app.sftp_menu = Some((side, index));
            Task::none()
        }
        Message::SftpCloseMenu => {
            app.sftp_menu = None;
            Task::none()
        }
        Message::SftpMenuDownload => {
            app.sftp_menu = None;
            start_download(app);
            Task::none()
        }
        Message::SftpMenuUpload => {
            app.sftp_menu = None;
            start_upload(app);
            Task::none()
        }
        Message::SftpMenuDelete => {
            // Confirm a delete over the whole current selection (right-click
            // already ensured the clicked row is part of it).
            if let Some((side, _index)) = app.sftp_menu.take() {
                if let Some(confirm) = build_delete_confirm(app, side) {
                    app.sftp_confirm = Some(confirm);
                }
            }
            Task::none()
        }
        Message::SftpConfirmDelete => {
            if let Some(confirm) = app.sftp_confirm.take() {
                match confirm.side {
                    crate::session::SftpSide::Remote => delete_remote_selected(app),
                    crate::session::SftpSide::Local => delete_local_selected(app),
                }
            }
            Task::none()
        }
        Message::SftpCancelDelete => {
            app.sftp_confirm = None;
            Task::none()
        }
        Message::SftpStartRename => {
            if let Some((side, index)) = app.sftp_menu.take() {
                let old = match (side, app.active_session()) {
                    (crate::session::SftpSide::Local, Some(s)) => {
                        s.local_files.get(index).map(|e| e.name.clone())
                    }
                    (crate::session::SftpSide::Remote, Some(s)) => {
                        s.remote_files.get(index).map(|e| e.name.clone())
                    }
                    _ => None,
                };
                if let Some(old) = old {
                    app.sftp_prompt = Some(crate::session::SftpPrompt {
                        side,
                        kind: crate::session::SftpPromptKind::Rename {
                            index,
                            old: old.clone(),
                        },
                        value: old,
                    });
                }
            }
            Task::none()
        }
        Message::SftpStartNewFolder(side) => {
            app.sftp_menu = None;
            app.sftp_prompt = Some(crate::session::SftpPrompt {
                side,
                kind: crate::session::SftpPromptKind::NewFolder,
                value: String::new(),
            });
            Task::none()
        }
        Message::SftpPromptChanged(v) => {
            if let Some(prompt) = app.sftp_prompt.as_mut() {
                prompt.value = v;
            }
            Task::none()
        }
        Message::SftpPromptCancel => {
            app.sftp_prompt = None;
            Task::none()
        }
        Message::SftpPromptConfirm => {
            sftp_prompt_confirm(app);
            Task::none()
        }

        Message::SftpMenuChmod => {
            if let Some((crate::session::SftpSide::Remote, idx)) = app.sftp_menu.take() {
                if let Some(session) = app.active_session_mut() {
                    if let Some(entry) = session.remote_files.get(idx) {
                        let raw = entry.permissions.unwrap_or(0o100644);
                        let mode = raw & 0o7777;
                        session.sftp_chmod = Some(crate::session::ChmodState {
                            path: entry.path.clone(),
                            current_mode: mode,
                            input: format!("{mode:o}"),
                        });
                    }
                }
            }
            Task::none()
        }
        Message::SftpChmodInput(v) => {
            if v.chars().all(|c| matches!(c, '0'..='7')) && v.len() <= 4 {
                if let Some(session) = app.active_session_mut() {
                    if let Some(chmod) = session.sftp_chmod.as_mut() {
                        chmod.input = v;
                    }
                }
            }
            Task::none()
        }
        Message::SftpChmodConfirm => {
            if let Some(session) = app.active_session_mut() {
                if let Some(chmod) = session.sftp_chmod.take() {
                    if let Ok(mode) = u32::from_str_radix(&chmod.input, 8) {
                        if let Some(tx) = &session.cmd_tx {
                            let _ = tx.try_send(crate::connection::Command::SftpChmod {
                                path: chmod.path,
                                mode,
                            });
                        }
                    }
                }
            }
            Task::none()
        }
        Message::SftpChmodCancel => {
            if let Some(session) = app.active_session_mut() {
                session.sftp_chmod = None;
            }
            Task::none()
        }

        // --- Settings ---
        Message::OpenSettings => {
            app.settings_open = true;
            app.palette_open = false;
            Task::none()
        }
        Message::CloseSettings => {
            app.settings_open = false;
            Task::none()
        }
        Message::SettingsDefaultUserChanged(v) => {
            app.default_user = v;
            app.persist_settings();
            Task::none()
        }
        Message::SettingsDefaultPortChanged(v) => {
            if v.chars().all(|c| c.is_ascii_digit()) && v.len() <= 5 {
                app.default_port = v;
                app.persist_settings();
            }
            Task::none()
        }
        Message::SettingsFontSize(delta) => {
            let next = (app.font_size as i16 + delta).clamp(
                crate::theme::MIN_FONT_SIZE as i16,
                crate::theme::MAX_FONT_SIZE as i16,
            ) as u16;
            if next != app.font_size {
                app.font_size = next;
                app.persist_settings();
                return apply_grid(app);
            }
            Task::none()
        }
        Message::SettingsPanelChanged(panel) => {
            app.settings_panel = panel;
            Task::none()
        }
        Message::SettingsServerAliveInterval(v) => {
            app.server_alive_interval = v;
            Task::none()
        }
        Message::SettingsOnDisconnect(v) => {
            app.on_disconnect = v;
            Task::none()
        }
        Message::SettingsColorScheme(scheme) => {
            app.color_scheme = scheme;
            crate::theme::set_scheme(scheme);
            if let Ok(store) = openterm_storage::WorkspaceStore::open(&app.db_path) {
                let mut settings = openterm_storage::UiSettings::default();
                settings.terminal_font_size = app.font_size;
                settings.color_scheme = scheme.to_str().to_string();
                let _ = store.save_ui_settings(&settings);
            }
            Task::none()
        }

        // --- Palette ---
        Message::TogglePalette => {
            app.palette_open = !app.palette_open;
            app.palette_query.clear();
            app.palette_selected = 0;
            Task::none()
        }
        Message::ClosePalette => {
            app.palette_open = false;
            app.palette_query.clear();
            app.palette_selected = 0;
            Task::none()
        }
        Message::PaletteQueryChanged(v) => {
            app.palette_query = v;
            app.palette_selected = 0;
            Task::none()
        }
        Message::PaletteMove(delta) => {
            let count = crate::palette::actions_for(app, &app.palette_query).len();
            if count > 0 {
                let cur = app.palette_selected as i32;
                let next = (cur + delta).rem_euclid(count as i32);
                app.palette_selected = next as usize;
            }
            Task::none()
        }
        Message::PaletteRunSelected => {
            let actions = crate::palette::actions_for(app, &app.palette_query);
            let chosen = actions.get(app.palette_selected).map(|a| a.message.clone());
            app.palette_open = false;
            app.palette_query.clear();
            app.palette_selected = 0;
            if let Some(message) = chosen {
                return update(app, message);
            }
            Task::none()
        }
        Message::PaletteRun(message) => {
            app.palette_open = false;
            app.palette_query.clear();
            app.palette_selected = 0;
            return update(app, *message);
        }

        Message::PointerMoved(_) => Task::none(),
        Message::PingTick => {
            let tasks: Vec<Task<Message>> = app
                .hosts
                .iter()
                .map(|host| {
                    let host_id = host.id;
                    let addr = format!("{}:{}", host.host, host.port);
                    Task::perform(
                        async move {
                            let start = std::time::Instant::now();
                            let ok = tokio::time::timeout(
                                std::time::Duration::from_secs(3),
                                tokio::net::TcpStream::connect(&addr),
                            )
                            .await;
                            match ok {
                                Ok(Ok(_)) => Some(start.elapsed().as_millis() as u32),
                                _ => None,
                            }
                        },
                        move |latency_ms| Message::PingResult { host_id, latency_ms },
                    )
                })
                .collect();
            Task::batch(tasks)
        }
        Message::PingResult { host_id, latency_ms } => {
            app.ping_results.insert(host_id, latency_ms);
            Task::none()
        }
        Message::Tick(now) => {
            app.now = now;
            Task::none()
        }

        // --- Worker events ---
        Message::Conn(event) => handle_conn_event(app, event),
    }
}

/// Mutate the active session's config, returning no task.
fn with_config<F: FnOnce(&mut SessionConfig)>(app: &mut App, f: F) -> Task<Message> {
    if let Some(session) = app.active_session_mut() {
        f(&mut session.config);
    }
    Task::none()
}

/// Load a config into the active session if it is idle, else into a new tab.
fn load_config_into_slot(app: &mut App, config: SessionConfig) {
    let reuse = app
        .active_session()
        .map(|s| !s.phase.is_active())
        .unwrap_or(false);
    if reuse {
        if let Some(session) = app.active_session_mut() {
            session.config = config;
            session.phase = Phase::Idle;
        }
    } else {
        let (cols, rows) = app.current_grid();
        app.spawn_session(config, cols, rows);
    }
}

/// One-click connect for a saved host: reuse the idle active session, or open
/// a new tab, then connect.
fn connect_in_new_or_active(app: &mut App, config: SessionConfig) -> Task<Message> {
    load_config_into_slot(app, config);
    connect_active(app)
}

/// Connect the active session.
fn connect_active(app: &mut App) -> Task<Message> {
    let (cols, rows) = app.current_grid();
    let Some(session) = app.active_session_mut() else {
        return Task::none();
    };
    if session.phase.is_active() {
        return Task::none();
    }
    let route = match App::build_route(&session.config) {
        Ok(route) => route,
        Err(error) => {
            session.phase = Phase::Failed(error.clone());
            session.status = error;
            return Task::none();
        }
    };
    let params = ConnectParams {
        route,
        cols,
        rows,
        term: "xterm-256color".to_string(),
    };
    session.resize_grid(cols, rows);
    session.clear_grid();
    session.phase = Phase::Connecting;
    session.status = "Connecting…".to_string();
    session.host_key = None;

    // If the worker channel is already live (reconnect), send now. Otherwise
    // park the params; the subscription will start the worker, which sends
    // `Ready`, and we dispatch the connect then.
    if let Some(tx) = &session.cmd_tx {
        let _ = tx.try_send(Command::Connect(params));
    } else {
        session.pending_connect = Some(params);
    }
    Task::none()
}

fn accept_host_key(app: &mut App) -> Task<Message> {
    let (cols, rows) = app.current_grid();
    let Some(session) = app.active_session_mut() else {
        return Task::none();
    };
    let Some(challenge) = session.host_key.take() else {
        return Task::none();
    };
    if let Err(error) = challenge.accept() {
        session.phase = Phase::Failed(format!("Could not trust host key: {error}"));
        session.status = "Host key error".to_string();
        return Task::none();
    }
    // Re-attempt the connection now that the key is trusted.
    let route = match App::build_route(&session.config) {
        Ok(route) => route,
        Err(error) => {
            session.phase = Phase::Failed(error.clone());
            session.status = error;
            return Task::none();
        }
    };
    let params = ConnectParams {
        route,
        cols,
        rows,
        term: "xterm-256color".to_string(),
    };
    session.phase = Phase::Connecting;
    session.status = "Connecting…".to_string();
    if let Some(tx) = &session.cmd_tx {
        let _ = tx.try_send(Command::Connect(params));
    } else {
        session.pending_connect = Some(params);
    }
    Task::none()
}

fn close_tab(app: &mut App, index: usize) -> Task<Message> {
    // `usize::MAX` is the "close the active tab" sentinel (Cmd+W).
    let index = if index == usize::MAX {
        app.active
    } else {
        index
    };
    if app.sessions.len() <= 1 {
        // Keep at least one tab; reset it to blank instead of removing.
        let config = app.blank_config();
        let (cols, rows) = app.current_grid();
        if let Some(session) = app.sessions.get_mut(0) {
            if let Some(tx) = &session.cmd_tx {
                let _ = tx.try_send(Command::Disconnect);
            }
        }
        app.sessions.clear();
        app.next_session_id = 1;
        app.spawn_session(config, cols, rows);
        return Task::none();
    }
    if index >= app.sessions.len() {
        return Task::none();
    }
    // Tell the worker to disconnect; dropping the session ends its subscription.
    if let Some(tx) = &app.sessions[index].cmd_tx {
        let _ = tx.try_send(Command::Disconnect);
    }
    app.sessions.remove(index);
    if app.active >= app.sessions.len() {
        app.active = app.sessions.len() - 1;
    } else if index < app.active {
        app.active -= 1;
    }
    Task::none()
}

/// Re-derive the grid from the current window and push a resize to every
/// connected session.
fn apply_grid(app: &mut App) -> Task<Message> {
    let (cols, rows) = app.current_grid();
    for session in &mut app.sessions {
        session.resize_grid(cols, rows);
        if session.phase.is_active() {
            if let Some(tx) = &session.cmd_tx {
                let _ = tx.try_send(Command::Resize { cols, rows });
            }
        }
    }
    Task::none()
}

fn sftp_refresh(app: &mut App) -> Task<Message> {
    if let Some(session) = app.active_session() {
        if session.sftp_open && session.phase == Phase::Connected {
            if let Some(tx) = &session.cmd_tx {
                let _ = tx.try_send(Command::SftpList(session.remote_path.clone()));
            }
        }
    }
    Task::none()
}

/// Re-read the active session's local pane using the current sort.
fn refresh_local_active(app: &mut App) {
    let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
    if let Some(session) = app.active_session_mut() {
        session.refresh_local(sort, asc);
    }
}

/// Start streamed downloads of all selected remote files into the local dir.
fn start_download(app: &mut App) {
    let mut next_id = app.next_transfer_id;
    let Some(session) = app.active_session() else {
        return;
    };
    let Some(tx) = &session.cmd_tx else {
        return;
    };
    // Collect first so we don't hold a borrow while bumping next_transfer_id.
    let mut queued = 0u64;
    for &i in &session.selected_remote {
        let Some(entry) = session.remote_files.get(i) else {
            continue;
        };
        let is_dir = matches!(entry.kind, openterm_ssh::RemoteFileKind::Directory);
        let _ = tx.try_send(Command::SftpDownload {
            id: next_id,
            name: entry.name.clone(),
            remote: join_remote(&session.remote_path, &entry.name),
            local: join_local(&session.local_path, &entry.name),
            // A directory's total is computed by the actor while walking it.
            size: if is_dir { 0 } else { entry.size.unwrap_or(0) },
            is_dir,
        });
        next_id += 1;
        queued += 1;
    }
    app.next_transfer_id += queued;
}

/// Start streamed uploads of all selected local files into the remote dir.
fn start_upload(app: &mut App) {
    let mut next_id = app.next_transfer_id;
    let Some(session) = app.active_session() else {
        return;
    };
    let Some(tx) = &session.cmd_tx else {
        return;
    };
    let mut queued = 0u64;
    for &i in &session.selected_local {
        let Some(entry) = session.local_files.get(i) else {
            continue;
        };
        let _ = tx.try_send(Command::SftpUpload {
            id: next_id,
            name: entry.name.clone(),
            local: entry.path.display().to_string(),
            remote: join_remote(&session.remote_path, &entry.name),
            // A directory's total is computed by the actor while walking it.
            size: if entry.is_dir { 0 } else { entry.size },
            is_dir: entry.is_dir,
        });
        next_id += 1;
        queued += 1;
    }
    app.next_transfer_id += queued;
}

/// Delete all selected remote entries over the live connection.
fn delete_remote_selected(app: &mut App) {
    if let Some(session) = app.active_session() {
        if let Some(tx) = &session.cmd_tx {
            for &i in &session.selected_remote {
                if let Some(entry) = session.remote_files.get(i) {
                    let path = join_remote(&session.remote_path, &entry.name);
                    let is_dir = matches!(entry.kind, openterm_ssh::RemoteFileKind::Directory);
                    let _ = tx.try_send(Command::SftpRemove { path, is_dir });
                }
            }
        }
    }
}

/// Delete all selected local entries from disk, then refresh the local pane.
fn delete_local_selected(app: &mut App) {
    let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
    if let Some(session) = app.active_session_mut() {
        // Collect paths first (indices shift once we refresh).
        let targets: Vec<(std::path::PathBuf, bool, String)> = session
            .selected_local
            .iter()
            .filter_map(|&i| session.local_files.get(i))
            .map(|e| (e.path.clone(), e.is_dir, e.name.clone()))
            .collect();
        let mut deleted = 0usize;
        let mut last_err = None;
        for (path, is_dir, _name) in &targets {
            let result = if *is_dir {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match result {
                Ok(()) => deleted += 1,
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        session.sftp_status = match (deleted, last_err) {
            (n, None) if n > 0 => format!("Deleted {n} item(s)"),
            (_, Some(e)) => format!("Delete failed: {e}"),
            _ => session.sftp_status.clone(),
        };
        session.refresh_local(sort, asc);
    }
}

/// Read the current modifiers into (toggle, range) for a select click.
/// Cmd/Ctrl = toggle; Shift = range.
fn select_mods(app: &App) -> (bool, bool) {
    let m = app.modifiers();
    let toggle = m.command() || m.control();
    let range = m.shift();
    (toggle, range)
}

/// Whether the remote row at `i` is a directory (decides double-click-to-enter).
fn is_remote_dir(app: &App, i: usize) -> bool {
    app.active_session()
        .and_then(|s| s.remote_files.get(i))
        .map(|e| matches!(e.kind, openterm_ssh::RemoteFileKind::Directory))
        .unwrap_or(false)
}

/// Whether the local row at `i` is a directory.
fn is_local_dir(app: &App, i: usize) -> bool {
    app.active_session()
        .and_then(|s| s.local_files.get(i))
        .map(|e| e.is_dir)
        .unwrap_or(false)
}

/// Enter the remote directory at row `i` (used by the double click and the
/// `SftpEnterRemote` message). No-op if the row isn't a directory.
fn enter_remote(app: &mut App, i: usize) -> Task<Message> {
    app.sftp_menu = None;
    let mut navigate = false;
    if let Some(session) = app.active_session_mut() {
        if let Some(entry) = session.remote_files.get(i) {
            if matches!(entry.kind, openterm_ssh::RemoteFileKind::Directory) {
                session.remote_path = join_remote(&session.remote_path, &entry.name);
                session.selected_remote.clear();
                session.remote_anchor = None;
                navigate = true;
            }
        }
    }
    if navigate {
        sftp_refresh(app)
    } else {
        Task::none()
    }
}

/// Enter the local directory at row `i`. No-op if the row isn't a directory.
fn enter_local(app: &mut App, i: usize) -> Task<Message> {
    app.sftp_menu = None;
    let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
    if let Some(session) = app.active_session_mut() {
        if let Some(entry) = session.local_files.get(i) {
            if entry.is_dir {
                session.local_path = entry.path.display().to_string();
                session.refresh_local(sort, asc);
            }
        }
    }
    Task::none()
}

/// Record a click on (side, index) and report whether it completes a
/// double-click. After a double-click the record resets, so a third quick
/// click starts a fresh single click rather than chaining.
fn register_click(app: &mut App, side: crate::session::SftpSide, index: usize) -> bool {
    const THRESHOLD: std::time::Duration = std::time::Duration::from_millis(400);
    let now = std::time::Instant::now();
    let double = detect_double_click(app.last_sftp_click, side, index, now, THRESHOLD);
    app.last_sftp_click = if double {
        None
    } else {
        Some((side, index, now))
    };
    double
}

/// Pure double-click decision: a click on (side, index) at `now` is a double
/// click when the previous recorded click was on the same row within `threshold`.
fn detect_double_click(
    last: Option<(crate::session::SftpSide, usize, std::time::Instant)>,
    side: crate::session::SftpSide,
    index: usize,
    now: std::time::Instant,
    threshold: std::time::Duration,
) -> bool {
    matches!(
        last,
        Some((s, i, t)) if s == side && i == index && now.saturating_duration_since(t) <= threshold
    )
}

/// Build a delete confirmation describing the active selection on `side`.
fn build_delete_confirm(
    app: &App,
    side: crate::session::SftpSide,
) -> Option<crate::session::SftpConfirm> {
    let session = app.active_session()?;
    let (label, count, any_dir) = match side {
        crate::session::SftpSide::Local => {
            let items: Vec<(&str, bool)> = session
                .selected_local
                .iter()
                .filter_map(|&i| session.local_files.get(i))
                .map(|e| (e.name.as_str(), e.is_dir))
                .collect();
            confirm_fields(&items)
        }
        crate::session::SftpSide::Remote => {
            let items: Vec<(&str, bool)> = session
                .selected_remote
                .iter()
                .filter_map(|&i| session.remote_files.get(i))
                .map(|e| {
                    (
                        e.name.as_str(),
                        matches!(e.kind, openterm_ssh::RemoteFileKind::Directory),
                    )
                })
                .collect();
            confirm_fields(&items)
        }
    };
    if count == 0 {
        return None;
    }
    Some(crate::session::SftpConfirm {
        side,
        label,
        count,
        any_dir,
    })
}

/// Derive (label, count, any_dir) from a list of (name, is_dir).
fn confirm_fields(items: &[(&str, bool)]) -> (String, usize, bool) {
    let count = items.len();
    let any_dir = items.iter().any(|(_, d)| *d);
    let label = match count {
        0 => String::new(),
        1 => items[0].0.to_string(),
        n => format!("{n} items"),
    };
    (label, count, any_dir)
}

/// Apply a confirmed new-folder / rename prompt.
fn sftp_prompt_confirm(app: &mut App) {
    let Some(prompt) = app.sftp_prompt.take() else {
        return;
    };
    let name = prompt.value.trim().to_string();
    if name.is_empty() {
        return;
    }
    let (sort, asc) = (app.sftp_sort, app.sftp_sort_asc);
    use crate::session::{SftpPromptKind, SftpSide};

    match prompt.side {
        SftpSide::Remote => {
            // Remote ops go through the connection actor.
            let Some(session) = app.active_session() else {
                return;
            };
            let Some(tx) = &session.cmd_tx else {
                return;
            };
            match prompt.kind {
                SftpPromptKind::NewFolder => {
                    let path = join_remote(&session.remote_path, &name);
                    let _ = tx.try_send(Command::SftpMkdir(path));
                }
                SftpPromptKind::Rename { old, .. } => {
                    let from = join_remote(&session.remote_path, &old);
                    let to = join_remote(&session.remote_path, &name);
                    let _ = tx.try_send(Command::SftpRename { from, to });
                }
            }
            // Refresh shortly after; the actor emits SftpDone which re-lists.
        }
        SftpSide::Local => {
            if let Some(session) = app.active_session_mut() {
                let base = std::path::Path::new(&session.local_path);
                let result = match &prompt.kind {
                    SftpPromptKind::NewFolder => std::fs::create_dir(base.join(&name)),
                    SftpPromptKind::Rename { old, .. } => {
                        std::fs::rename(base.join(old), base.join(&name))
                    }
                };
                session.sftp_status = match result {
                    Ok(()) => "Done".to_string(),
                    Err(e) => format!("Failed: {e}"),
                };
                session.refresh_local(sort, asc);
            }
        }
    }
}

fn handle_conn_event(app: &mut App, event: ConnEvent) -> Task<Message> {
    let session_id = event.session_id();
    let Some(index) = app.session_index_by_id(session_id) else {
        return Task::none();
    };
    let smoke = app.smoke_status.clone();
    let app_smoke_sftp_pending = app.smoke_sftp_pending;
    let (sort, sort_asc) = (app.sftp_sort, app.sftp_sort_asc);
    let app_download_pending = app.smoke_download_pending;
    let app_open_menu = app.smoke_open_menu;
    let mut open_sftp_after = false;
    let mut open_menu_after = false;
    let mut download_after: Option<usize> = None;
    let mut touch_host_id = None;
    let session = &mut app.sessions[index];

    match event {
        ConnEvent::Ready { sender, .. } => {
            session.cmd_tx = Some(sender.clone());
            if let Some(params) = session.pending_connect.take() {
                let _ = sender.try_send(Command::Connect(params));
            }
        }
        ConnEvent::Connecting { .. } => {
            session.phase = Phase::Connecting;
            session.status = "Connecting…".to_string();
        }
        ConnEvent::Connected { .. } => {
            session.phase = Phase::Connected;
            session.status = "Connected".to_string();
            session.host_key = None;
            crate::smoke::record(&smoke, "connected");
            touch_host_id = session.config.host_id;
        }
        ConnEvent::Output { bytes, .. } => {
            session.write_output(&bytes);
            crate::smoke::record(&smoke, "output");
            // Smoke: once the shell is producing output, open SFTP once to
            // exercise the GUI list path end-to-end. Deferred until after the
            // `session` borrow ends (see bottom of fn).
            if app_smoke_sftp_pending {
                open_sftp_after = true;
            }
        }
        ConnEvent::HostKeyRequired { challenge, .. } => {
            session.host_key = Some(*challenge);
            session.phase = Phase::Connecting;
            session.status = "Host key confirmation required".to_string();
        }
        ConnEvent::SftpListed { path, result, .. } => match result {
            Ok(mut entries) => {
                crate::session::sort_remote(&mut entries, sort, sort_asc);
                session.remote_files = entries;
                session.remote_path = path;
                session.selected_remote.clear();
                session.remote_anchor = None;
                session.sftp_status = format!("{} items", session.remote_files.len());
                crate::smoke::record(&smoke, "sftp_listed");
                // Smoke: download the first regular file to exercise transfers.
                if app_download_pending {
                    download_after = session
                        .remote_files
                        .iter()
                        .position(|e| !matches!(e.kind, openterm_ssh::RemoteFileKind::Directory));
                }
                if app_open_menu && !session.remote_files.is_empty() {
                    open_menu_after = true;
                }
            }
            Err(error) => {
                session.sftp_status = format!("List failed: {error}");
                crate::smoke::record(&smoke, "sftp_failed");
            }
        },
        ConnEvent::SftpDone { message, .. } => {
            session.sftp_status = match message {
                Ok(msg) => msg,
                Err(error) => format!("Failed: {error}"),
            };
            session.refresh_local(sort, sort_asc);
            return sftp_refresh(app);
        }
        ConnEvent::TransferStarted {
            id,
            name,
            direction,
            total,
            ..
        } => {
            crate::smoke::record(&smoke, "transfer_started");
            session.transfers.insert(
                0,
                crate::session::Transfer {
                    id,
                    name,
                    direction,
                    total,
                    transferred: 0,
                    speed_bps: 0.0,
                    status: crate::session::TransferStatus::Active,
                },
            );
            // Cap history length.
            session.transfers.truncate(40);
        }
        ConnEvent::TransferProgress {
            id,
            transferred,
            speed_bps,
            ..
        } => {
            if let Some(t) = session.transfers.iter_mut().find(|t| t.id == id) {
                t.transferred = transferred;
                t.speed_bps = speed_bps;
            }
        }
        ConnEvent::TransferFinished { id, result, .. } => {
            crate::smoke::record(&smoke, "transfer_finished");
            if let Some(t) = session.transfers.iter_mut().find(|t| t.id == id) {
                match result {
                    Ok(bytes) => {
                        t.transferred = bytes;
                        if t.total == 0 {
                            t.total = bytes;
                        }
                        t.speed_bps = 0.0;
                        t.status = crate::session::TransferStatus::Done;
                    }
                    Err(e) => t.status = crate::session::TransferStatus::Failed(e),
                }
            }
            // A finished transfer changes both panes (new file present).
            session.refresh_local(sort, sort_asc);
            return sftp_refresh(app);
        }
        ConnEvent::Metrics { raw, .. } => {
            let now = std::time::Instant::now();
            let sample = crate::metrics::parse_sample(&raw);
            let computed = crate::metrics::compute(&sample, session.prev_sample.as_ref(), now);
            // Feed the rail's line charts (only once rates are meaningful, so
            // the first frame's 0% CPU / huge rate spike doesn't distort them).
            if computed.has_rates {
                session.push_metrics_history(
                    computed.cpu_percent,
                    computed.mem_percent,
                    (computed.net_rx_bps + computed.net_tx_bps) as f32,
                    (computed.disk_read_bps + computed.disk_write_bps) as f32,
                );
            }
            session.metrics = Some(computed);
            session.prev_sample = Some((now, sample));
        }
        ConnEvent::Processes { raw, .. } => {
            let mut procs = crate::metrics::parse_processes(&raw);
            if let Some(panel) = session.monitor_panel {
                crate::metrics::sort_processes(&mut procs, panel.sort());
            }
            session.processes = procs;
        }
        ConnEvent::Exit { code, .. } => {
            session.status = format!("Process exited ({code})");
        }
        ConnEvent::Closed { .. } => {
            session.phase = Phase::Idle;
            session.status = "Disconnected".to_string();
            session.monitor_panel = None;
            session.processes.clear();
        }
        ConnEvent::Failed { error, .. } => {
            crate::smoke::record(&smoke, "failed");
            session.phase = Phase::Failed(error.clone());
            session.status = error;
        }
    }
    // Borrow of `session` ends here; safe to touch `app` again.
    if let Some(host_id) = touch_host_id {
        app.touch_host(host_id);
    }
    if open_sftp_after {
        app.smoke_sftp_pending = false;
        return update(app, Message::ToggleSftp);
    }
    if let Some(file_index) = download_after {
        app.smoke_download_pending = false;
        if let Some(session) = app.active_session_mut() {
            session.selected_remote.clear();
            session.selected_remote.insert(file_index);
        }
        start_download(app);
    }
    if open_menu_after {
        app.smoke_open_menu = false;
        app.sftp_menu = Some((crate::session::SftpSide::Remote, 0));
    }
    Task::none()
}

// --- remote path helpers ---

fn join_remote(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", base.trim_end_matches('/'))
    }
}

fn parent_remote(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
        None => ".".to_string(),
    }
}

fn join_local(base: &str, name: &str) -> String {
    std::path::Path::new(base).join(name).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::detect_double_click;
    use crate::session::SftpSide;
    use std::time::{Duration, Instant};

    #[test]
    fn double_click_requires_same_row_within_threshold() {
        let t0 = Instant::now();
        let threshold = Duration::from_millis(400);

        // Second click on the same row, soon after → double click.
        assert!(detect_double_click(
            Some((SftpSide::Remote, 3, t0)),
            SftpSide::Remote,
            3,
            t0 + Duration::from_millis(200),
            threshold,
        ));

        // Same row but too slow → just two single clicks.
        assert!(!detect_double_click(
            Some((SftpSide::Remote, 3, t0)),
            SftpSide::Remote,
            3,
            t0 + Duration::from_millis(600),
            threshold,
        ));

        // A different row resets the gesture.
        assert!(!detect_double_click(
            Some((SftpSide::Remote, 3, t0)),
            SftpSide::Remote,
            4,
            t0 + Duration::from_millis(100),
            threshold,
        ));

        // The same index on the other pane is not the same row.
        assert!(!detect_double_click(
            Some((SftpSide::Remote, 3, t0)),
            SftpSide::Local,
            3,
            t0 + Duration::from_millis(100),
            threshold,
        ));

        // No prior click → never a double click.
        assert!(!detect_double_click(
            None,
            SftpSide::Remote,
            3,
            t0 + Duration::from_millis(100),
            threshold,
        ));
    }
}
