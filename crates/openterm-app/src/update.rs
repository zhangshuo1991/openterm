//! Message handling. Translates UI/worker messages into state changes and
//! tasks. Connection lifecycle is the delicate part — see the `Conn` arm.

use iced::{animation::Animation, clipboard, Task};

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
                    if let Some(store) = app.store() {
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
                // `reserved_top`/`reserved_right` key off the *active* session's
                // phase, so switching between an idle and a connected tab moves
                // the subtab band and rail — shrinking the canvas. Re-derive the
                // grid for the newly-active session or its bottom rows (and the
                // cursor) render below the canvas and get clipped, the same
                // no-cursor / no-input symptom as connecting.
                return apply_grid(app);
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
                        if let Some(store) = app.store() {
                            app.hosts = store.list_hosts().unwrap_or_default();
                        }
                        app.touch_vault();
                        let label = {
                            let name = config.name.trim();
                            if name.is_empty() {
                                config.host.trim().to_string()
                            } else {
                                name.to_string()
                            }
                        };
                        app.status = format!("Host \"{label}\" saved to the sidebar.");
                        app.push_toast(
                            crate::ui::toasts::ToastKind::Success,
                            format!("Saved \"{label}\" to the sidebar"),
                        );
                    }
                    Err(error) => {
                        app.status = format!("Save failed: {error}");
                        app.push_toast(
                            crate::ui::toasts::ToastKind::Error,
                            format!("Couldn't save host: {error}"),
                        );
                    }
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
        Message::TerminalInput(bytes) => terminal_input(app, bytes),
        Message::TerminalWriteRaw(bytes) => {
            // Straight to the PTY — no snippet/suggestion/scroll side effects.
            if let Some(session) = app.active_session() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Write(bytes));
                }
            }
            Task::none()
        }
        Message::OpenUrl(url) => {
            // Only allow http/https to reach the OS opener, so a crafted
            // "file://" or other scheme in remote output can't trigger
            // something unexpected on click.
            if url.starts_with("http://") || url.starts_with("https://") {
                #[cfg(target_os = "macos")]
                let _ = std::process::Command::new("open").arg(&url).spawn();
                #[cfg(all(unix, not(target_os = "macos")))]
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                app.status = format!("Opening {url}");
            }
            Task::none()
        }
        Message::TerminalSelectAll => {
            app.terminal_menu = None;
            // Select the whole visible grid: (0,0) to (last_col, last_row).
            if let Some(session) = app.active_session_mut() {
                let snap = session.render.snapshot(&session.terminal);
                let last_row = snap.cells.len().saturating_sub(1);
                let last_col = snap
                    .cells
                    .get(last_row)
                    .map(|r| r.len().saturating_sub(1))
                    .unwrap_or(0);
                session.selection = Some((0, 0, last_col, last_row));
            }
            Task::none()
        }
        Message::TerminalOpenMenu(x, y) => {
            app.terminal_menu = Some((x, y));
            Task::none()
        }
        Message::TerminalCloseMenu => {
            app.terminal_menu = None;
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
        Message::PasteRequested => {
            app.terminal_menu = None;
            clipboard::read().map(Message::PasteReady)
        }
        Message::SelectionChanged(sel) => {
            // A fresh click/drag on the canvas also dismisses the context menu.
            app.terminal_menu = None;
            if let Some(session) = app.active_session_mut() {
                session.selection = sel;
            }
            Task::none()
        }
        Message::TerminalCopy => {
            app.terminal_menu = None;
            let text = app.active_session().and_then(|s| {
                let (c1, r1, c2, r2) = s.selection?;
                let snap = s.render.snapshot(&s.terminal);
                let t = crate::terminal_render::selected_text(&snap, (c1, r1), (c2, r2));
                if t.is_empty() { None } else { Some(t) }
            });
            if let Some(t) = text {
                let chars = t.chars().count();
                app.push_toast(
                    crate::ui::toasts::ToastKind::Success,
                    format!("Copied {chars} chars"),
                );
                // Brief selection flash for visual confirmation.
                app.now = std::time::Instant::now();
                app.copy_flash_until =
                    Some(app.now + std::time::Duration::from_millis(450));
                clipboard::write(t)
            } else {
                Task::none()
            }
        }
        Message::PasteReady(Some(text)) => {
            if let Some(session) = app.active_session_mut() {
                if let Some(tx) = &session.cmd_tx {
                    // Honor the remote app's bracketed-paste mode (2004):
                    // wrap for TUIs/shells that requested it, plain bytes
                    // (with \n → \r) otherwise.
                    let bracketed = session.terminal.bracketed_paste();
                    let _ = tx.try_send(Command::Write(crate::keys::paste_bytes(&text, bracketed)));
                    // Pasting is input: snap back to the live bottom.
                    session.terminal.scroll_to_bottom();
                }
            }
            Task::none()
        }
        Message::PasteReady(None) => Task::none(),
        Message::ClearTerminal => {
            app.terminal_menu = None;
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
            let now = std::time::Instant::now();
            app.history_anim.go_mut(app.history_open, now);
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
            if let Some(store) = app.store() {
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
                let width = (app.window_size.width - point.x - crate::ui::HISTORY_DIVIDER_WIDTH)
                    .clamp(200.0, 520.0);
                app.history_width = width;
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
            let now = std::time::Instant::now();
            app.sidebar_anim.go_mut(!app.sidebar_collapsed, now);
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
            }
            Task::none()
        }
        Message::SidebarDragEnd => {
            app.sidebar_dragging = false;
            apply_grid(app)
        }

        Message::RailDragStart => {
            app.rail_dragging = true;
            Task::none()
        }
        Message::RailDragMove(point) => {
            if app.rail_dragging {
                let w = (app.window_size.width - point.x).clamp(180.0, 520.0);
                app.rail_width = w;
            }
            Task::none()
        }
        Message::RailDragEnd => {
            app.rail_dragging = false;
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
            // Sample while the active session is connected. Cadence is decided
            // by the subscription (2s with the rail visible, 10s collapsed).
            // Process sampling only runs while the expander is open.
            if let Some(session) = app.active_session() {
                if session.phase == Phase::Connected {
                    if let Some(tx) = &session.cmd_tx {
                        let _ = tx.try_send(Command::SampleMetrics);
                        if session.monitor_panel.is_some() {
                            let _ = tx.try_send(Command::SampleProcesses);
                            let _ = tx.try_send(Command::SamplePorts);
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
        Message::SftpSetSort(side, field) => {
            use crate::session::SftpSide;
            let (cur_sort, cur_asc) = match side {
                SftpSide::Local => (app.sftp_sort_local, app.sftp_sort_asc_local),
                SftpSide::Remote => (app.sftp_sort, app.sftp_sort_asc),
            };
            let (new_sort, new_asc) = if cur_sort == field {
                (field, !cur_asc)
            } else {
                (field, true)
            };
            match side {
                SftpSide::Local => {
                    app.sftp_sort_local = new_sort;
                    app.sftp_sort_asc_local = new_asc;
                }
                SftpSide::Remote => {
                    app.sftp_sort = new_sort;
                    app.sftp_sort_asc = new_asc;
                }
            }
            if let Some(session) = app.active_session_mut() {
                match side {
                    SftpSide::Local => {
                        crate::session::sort_local(&mut session.local_files, new_sort, new_asc);
                        session.selected_local.clear();
                        session.local_anchor = None;
                    }
                    SftpSide::Remote => {
                        crate::session::sort_remote(&mut session.remote_files, new_sort, new_asc);
                        session.selected_remote.clear();
                        session.remote_anchor = None;
                    }
                }
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
            // Double-click a file → open in the file viewer.
            if double && !toggle && !range && !is_remote_dir(app, i) {
                if let Some(session) = app.active_session() {
                    if let Some(entry) = session.remote_files.get(i) {
                        let path = join_remote(&session.remote_path, &entry.name);
                        return open_file_viewer(app, path);
                    }
                }
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
        Message::TransferPause(id) => {
            // Optimistic feedback: mark "Pausing…" immediately so the row reacts
            // to the click even though the worker must drain in-flight chunks
            // before it confirms with TransferPaused.
            if let Some(session) = app.active_session_mut() {
                if let Some(t) = session.transfers.iter_mut().find(|t| t.id == id) {
                    if t.status == crate::session::TransferStatus::Active {
                        t.pause_requested = true;
                    }
                }
            }
            transfer_control(app, id, crate::connection::Command::SftpPauseTransfer { id });
            Task::none()
        }
        Message::TransferCancel(id) => {
            // Optimistic feedback: drop the row now; the worker removes the
            // `.part` and emits TransferCanceled (a no-op for the already-gone
            // row). Works whether the transfer is active or paused.
            if let Some(session) = app.active_session_mut() {
                session.transfers.retain(|t| t.id != id);
            }
            transfer_control(app, id, crate::connection::Command::SftpCancelTransfer { id });
            Task::none()
        }
        Message::TransferResume(id) => {
            resume_transfer(app, id);
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
            let now = std::time::Instant::now();
            app.settings_anim = Animation::new(false).quick();
            app.settings_anim.go_mut(true, now);
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
            app.persist_settings();
            Task::none()
        }
        Message::SettingsCursorShape(shape) => {
            app.cursor_shape = shape;
            app.persist_settings();
            Task::none()
        }
        Message::SettingsLineHeight(delta) => {
            let next = (app.line_height + delta).clamp(1.0, 2.0);
            // Round to 2 decimals so repeated nudges don't drift.
            let next = (next * 100.0).round() / 100.0;
            if (next - app.line_height).abs() > f32::EPSILON {
                app.line_height = next;
                crate::terminal_render::set_line_height(next);
                app.persist_settings();
                return apply_grid(app);
            }
            Task::none()
        }
        Message::SettingsLetterSpacing(delta) => {
            let next = (app.letter_spacing + delta).clamp(0.0, 6.0);
            let next = (next * 10.0).round() / 10.0;
            if (next - app.letter_spacing).abs() > f32::EPSILON {
                app.letter_spacing = next;
                crate::terminal_render::set_letter_spacing(next);
                app.persist_settings();
                return apply_grid(app);
            }
            Task::none()
        }
        Message::SettingsAccent(hex) => {
            crate::theme::set_accent_override(&hex);
            app.accent_hex = hex;
            app.persist_settings();
            Task::none()
        }
        Message::GroupToggle(name) => {
            if !app.collapsed_groups.remove(&name) {
                app.collapsed_groups.insert(name);
            }
            app.persist_settings();
            Task::none()
        }
        Message::HistoryRun(cmd) => {
            let bytes = format!("{cmd}\n").into_bytes();
            if let Some(s) = app.active_session() {
                if let Some(tx) = &s.cmd_tx {
                    let _ = tx.try_send(Command::Write(bytes));
                }
            }
            Task::none()
        }
        Message::HistoryToggleExpand(ts) => {
            if !app.expanded_history.remove(&ts) {
                app.expanded_history.insert(ts);
            }
            Task::none()
        }
        Message::ToastDismiss(id) => {
            if let Some(t) = app.toasts.iter_mut().find(|t| t.id == id) {
                t.dismissed = true;
            }
            Task::none()
        }

        // --- Sprint 3: snippets + history search ---
        Message::SnippetDraftAbbr(v) => {
            app.snippet_draft_abbr = v;
            Task::none()
        }
        Message::SnippetDraftExpansion(v) => {
            app.snippet_draft_expansion = v;
            Task::none()
        }
        Message::SnippetAdd => {
            let abbr = app.snippet_draft_abbr.trim().to_string();
            let expansion = app.snippet_draft_expansion.trim().to_string();
            // Both halves are required; the abbr can't contain whitespace since
            // it's matched against a single typed token before Space/Tab.
            if abbr.is_empty() || expansion.is_empty() || abbr.contains(char::is_whitespace) {
                app.push_toast(
                    crate::ui::toasts::ToastKind::Warning,
                    "Snippet needs a whitespace-free abbreviation and an expansion",
                );
                return Task::none();
            }
            let snippet = openterm_storage::Snippet { abbr, expansion };
            if let Some(store) = app.store() {
                let _ = store.save_snippet(&snippet);
                app.snippets = store.list_snippets().unwrap_or_default();
            }
            app.snippet_draft_abbr.clear();
            app.snippet_draft_expansion.clear();
            Task::none()
        }
        Message::SnippetDelete(abbr) => {
            if let Some(store) = app.store() {
                let _ = store.delete_snippet(&abbr);
                app.snippets = store.list_snippets().unwrap_or_default();
            }
            Task::none()
        }
        Message::HistorySearchOpen => {
            app.history_search_open = true;
            app.history_search_query.clear();
            app.history_search_idx = 0;
            iced::widget::operation::focus(crate::ui::history_search::INPUT_ID.clone())
        }
        Message::HistorySearchClose => {
            app.history_search_open = false;
            app.history_search_query.clear();
            app.history_search_idx = 0;
            Task::none()
        }
        Message::HistorySearchQuery(v) => {
            app.history_search_query = v;
            app.history_search_idx = 0;
            Task::none()
        }
        Message::HistorySearchMove(delta) => {
            let count = crate::ui::history_search::matches(app).len();
            if count > 0 {
                let cur = app.history_search_idx as i32;
                app.history_search_idx = (cur + delta).rem_euclid(count as i32) as usize;
            }
            Task::none()
        }
        Message::HistorySearchAccept => {
            let cmd = crate::ui::history_search::matches(app)
                .get(app.history_search_idx)
                .map(|s| s.to_string());
            app.history_search_open = false;
            app.history_search_query.clear();
            app.history_search_idx = 0;
            if let Some(cmd) = cmd {
                // Insert onto the prompt without a trailing newline, mirroring
                // the shell's own Ctrl+R (the user reviews, then presses Enter).
                if let Some(session) = app.active_session_mut() {
                    session.track_input(cmd.as_bytes());
                    session.inline_suggestion = None;
                }
                if let Some(session) = app.active_session() {
                    if let Some(tx) = &session.cmd_tx {
                        let _ = tx.try_send(Command::Write(cmd.into_bytes()));
                    }
                }
            }
            Task::none()
        }
        Message::ToggleRevealPassword => {
            app.reveal_password = !app.reveal_password;
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
            // With the sidebar collapsed nothing shows per-host latency except
            // the footer's sparkline for the active session's host — skip
            // pinging every other saved host until the sidebar is back.
            let active_host = app.active_session().and_then(|s| s.config.host_id);
            let sidebar_visible = !app.sidebar_collapsed();
            let tasks: Vec<Task<Message>> = app
                .hosts
                .iter()
                .filter(|host| sidebar_visible || Some(host.id) == active_host)
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
            // Keep a short rolling history per host for the footer sparkline.
            // Unreachable samples record as 0 so gaps still show on the chart.
            let hist = app.ping_history.entry(host_id).or_default();
            hist.push_back(latency_ms.unwrap_or(0).min(u16::MAX as u32) as u16);
            while hist.len() > 30 {
                hist.pop_front();
            }
            Task::none()
        }
        Message::Tick(now) => {
            app.now = now;

            // Retire finished / dismissed toasts (progress is wall-clock
            // derived, so there's nothing to advance here).
            if !app.toasts.is_empty() {
                app.toasts.retain(|t| !t.done(now));
            }

            // Finalize tab-close animations: once a tab has fully collapsed,
            // drop the session. Keyed by stable id, so concurrent closes are safe.
            if !app.closing_tabs.is_empty() {
                let done: Vec<u64> = app
                    .closing_tabs
                    .iter()
                    .filter(|(_, anim)| !anim.is_animating(now))
                    .map(|(id, _)| *id)
                    .collect();
                for id in done {
                    app.closing_tabs.remove(&id);
                    finalize_tab_close(app, id);
                }
            }
            // Sidebar / history / rail slides change the terminal's available
            // width every frame. Those toggles call `apply_grid` at t=0, when
            // the animated width is still the *old* value, so the grid would
            // otherwise stay sized for the pre-animation layout until the next
            // window resize. Re-derive each frame so the grid tracks the slide
            // and lands correct on the final frame. `resize_grid` no-ops when
            // the size is unchanged, so a steady frame (e.g. a toast fading)
            // costs nothing.
            return apply_grid(app);
        }
        Message::PulseTick => {
            app.connecting_pulse = !app.connecting_pulse;
            Task::none()
        }

        // --- File viewer ---
        Message::OpenFileViewer(path) => open_file_viewer(app, path),
        Message::FileViewerClose => {
            if let Some(s) = app.active_session_mut() { s.file_viewer = None; }
            Task::none()
        }
        Message::FileViewerChunk { offset, data, total } => file_viewer_chunk(app, offset, data, total),
        Message::FileViewerToggleEdit => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    let entering_edit = fv.mode != crate::session::ViewerMode::Edit;
                    fv.mode = if entering_edit {
                        // Populate the text_editor from current content.
                        // Normalize CRLF/CR → LF first: iced's editor backend
                        // (cosmic-text) treats a lone '\r' as its own line
                        // break, so a '\r\n' file would render a blank line
                        // between every real line ("大量无效换行"). We edit in
                        // LF and the shell/editors handle LF fine.
                        let raw = match &fv.content {
                            crate::session::ViewerContent::Loaded(t) => t.clone(),
                            _ => String::new(),
                        };
                        let text = raw.replace("\r\n", "\n").replace('\r', "\n");
                        fv.editor = iced::widget::text_editor::Content::with_text(&text);
                        crate::session::ViewerMode::Edit
                    } else {
                        crate::session::ViewerMode::Preview
                    };
                }
            }
            Task::none()
        }
        Message::FileViewerAction(action) => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    let is_edit = matches!(action, iced::widget::text_editor::Action::Edit(_));
                    fv.editor.perform(action);
                    if is_edit { fv.dirty = true; }
                }
            }
            Task::none()
        }
        Message::FileViewerTextChanged(_) => Task::none(), // superseded by FileViewerAction
        Message::FileViewerSearchChanged(q) => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    fv.search = q;
                    fv.refresh_matches();
                }
            }
            Task::none()
        }
        Message::FileViewerReplaceChanged(r) => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer { fv.replace = r; }
            }
            Task::none()
        }
        Message::FileViewerSearchNext => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    if !fv.matches.is_empty() {
                        fv.match_idx = (fv.match_idx + 1) % fv.matches.len();
                    }
                }
            }
            Task::none()
        }
        Message::FileViewerSearchPrev => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    if !fv.matches.is_empty() {
                        fv.match_idx = fv.match_idx.checked_sub(1).unwrap_or(fv.matches.len() - 1);
                    }
                }
            }
            Task::none()
        }
        Message::FileViewerReplaceOne => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    if let crate::session::ViewerContent::Loaded(ref mut c) = fv.content {
                        if let Some(&off) = fv.matches.get(fv.match_idx) {
                            let end = off + fv.search.len();
                            // Byte-offset safety: `get` returns None unless both
                            // ends are valid char boundaries, and comparing the
                            // slice against the search text rejects any stale
                            // offset (multi-byte text made this class of code
                            // panic before — see the UTF-8 slicing history).
                            if c.get(off..end) == Some(fv.search.as_str()) {
                                c.replace_range(off..end, &fv.replace.clone());
                                fv.dirty = true;
                                fv.refresh_matches();
                            }
                        }
                    }
                }
            }
            Task::none()
        }
        Message::FileViewerReplaceAll => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    if let crate::session::ViewerContent::Loaded(ref mut c) = fv.content {
                        if !fv.search.is_empty() {
                            *c = c.replace(&fv.search.clone(), &fv.replace.clone());
                            fv.dirty = true;
                            fv.refresh_matches();
                        }
                    }
                }
            }
            Task::none()
        }
        Message::FileViewerSave => file_viewer_save(app),
        Message::FileViewerSaved(result) => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer {
                    fv.saving = false;
                    if result.is_ok() { fv.dirty = false; }
                }
            }
            Task::none()
        }
        Message::FileViewerNextPage => file_viewer_page(app, true),
        Message::FileViewerPrevPage => file_viewer_page(app, false),
        Message::FileViewerScroll(v) => {
            if let Some(s) = app.active_session_mut() {
                if let Some(fv) = &mut s.file_viewer { fv.scroll = v; }
            }
            Task::none()
        }

        // --- Worker events ---
        Message::Conn(event) => handle_conn_event(app, event),

        // --- Terminal search (Cmd+F) ---
        Message::TerminalSearchOpen => {
            app.terminal_search = Some(app.terminal_search.take().unwrap_or_default());
            app.terminal_search_idx = 0;
            iced::widget::operation::focus(crate::ui::terminal::SEARCH_INPUT_ID.clone())
        }
        Message::TerminalSearchQuery(value) => {
            app.terminal_search = Some(value);
            app.terminal_search_idx = 0;
            Task::none()
        }
        Message::TerminalSearchNext => {
            // Unbounded; the canvas wraps it modulo the live match count.
            app.terminal_search_idx = app.terminal_search_idx.wrapping_add(1);
            Task::none()
        }
        Message::TerminalSearchPrev => {
            // Match count lives in the render layer, so we can't wrap to the
            // last match here; stop at the first instead of underflowing.
            app.terminal_search_idx = app.terminal_search_idx.saturating_sub(1);
            Task::none()
        }
        Message::TerminalSearchClose => {
            app.terminal_search = None;
            app.terminal_search_idx = 0;
            Task::none()
        }

        // --- Vault master password ---
        Message::VaultPasswordInput(value) => {
            app.vault_pw = value;
            app.vault_err = None;
            Task::none()
        }
        Message::VaultConfirmInput(value) => {
            app.vault_confirm = value;
            app.vault_err = None;
            Task::none()
        }
        Message::VaultSubmit => vault_submit(app),
        Message::VaultLock => {
            app.vault_master = None;
            app.vault_pw.clear();
            app.vault_confirm.clear();
            app.status = "Vault locked.".to_string();
            Task::none()
        }
        Message::VaultCheckLock => {
            // Auto-lock on inactivity, and detect system sleep via a timer gap:
            // if far more than the 60s tick has elapsed, the machine likely slept.
            let now = std::time::Instant::now();
            let since_check = now.duration_since(app.vault_last_check);
            app.vault_last_check = now;
            if app.vault_locked() {
                return Task::none();
            }
            let slept = since_check > std::time::Duration::from_secs(150);
            let idle = now.duration_since(app.vault_last_use) > VAULT_IDLE_TIMEOUT;
            if slept || idle {
                app.vault_master = None;
                app.status = if slept {
                    "Vault locked (system resumed from sleep).".to_string()
                } else {
                    "Vault locked (inactive).".to_string()
                };
            }
            Task::none()
        }
        Message::VaultUnlockResult(result) => {
            match result {
                Ok(master) => {
                    app.vault_master = Some(master);
                    app.vault_pw.clear();
                    app.vault_confirm.clear();
                    app.vault_err = None;
                    app.touch_vault();
                    app.vault_last_check = std::time::Instant::now();
                    app.status = "Vault unlocked.".to_string();
                    // Refresh hosts now that secrets can be decrypted.
                    if let Some(store) = app.store() {
                        app.hosts = store.list_hosts().unwrap_or_default();
                    }
                }
                Err(reason) => {
                    app.vault_pw.clear();
                    app.vault_err = Some(reason);
                }
            }
            Task::none()
        }
        Message::VaultSetupResult(result) => {
            match result {
                Ok(master) => {
                    // Canary stored under the new master password. Now migrate
                    // every saved secret from the default key to the master
                    // password before flipping the vault on.
                    app.vault_master = Some(master.clone());
                    app.vault_pw.clear();
                    app.vault_confirm.clear();
                    app.vault_err = None;
                    app.vault_busy = true;
                    app.status = "Encrypting saved credentials…".to_string();
                    let Some(store) = app.store() else {
                        app.vault_busy = false;
                        app.vault_err = Some("Workspace storage is unavailable.".to_string());
                        return Task::none();
                    };
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                reencrypt_all_secrets(&store, crate::VAULT_DEFAULT_KEY, master.as_bytes())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                        },
                        Message::VaultEnableResult,
                    );
                }
                Err(reason) => {
                    app.vault_err = Some(reason);
                }
            }
            Task::none()
        }
        Message::VaultEnableRequest => {
            if app.vault_enabled || app.vault_busy {
                return Task::none();
            }
            // Open the create-master-password dialog.
            app.vault_setup_prompt = true;
            app.vault_pw.clear();
            app.vault_confirm.clear();
            app.vault_err = None;
            iced::widget::operation::focus(crate::ui::vault::PW_INPUT_ID.clone())
        }
        Message::VaultEnableResult(result) => {
            app.vault_busy = false;
            match result {
                Ok(()) => {
                    app.vault_enabled = true;
                    app.vault_has_canary = true;
                    app.vault_setup_prompt = false;
                    app.touch_vault();
                    app.vault_last_check = std::time::Instant::now();
                    app.persist_settings();
                    if let Some(store) = app.store() {
                        app.hosts = store.list_hosts().unwrap_or_default();
                    }
                    app.status = "Credential vault enabled.".to_string();
                }
                Err(reason) => {
                    // Migration failed: roll back so we don't strand secrets
                    // encrypted under a master password the vault won't use.
                    app.vault_master = None;
                    app.vault_setup_prompt = false;
                    if let Some(store) = app.store() {
                        let _ = store.delete_master_canary();
                    }
                    app.vault_has_canary = false;
                    app.vault_err = Some(reason.clone());
                    app.status = format!("Could not enable vault: {reason}");
                }
            }
            Task::none()
        }
        Message::VaultDisableRequest => {
            if !app.vault_enabled || app.vault_busy {
                return Task::none();
            }
            // Need the master password in memory to re-encrypt back to default.
            let Some(master) = app.vault_master.clone() else {
                app.status = "Unlock the vault before disabling it.".to_string();
                return Task::none();
            };
            app.vault_busy = true;
            app.status = "Decrypting saved credentials…".to_string();
            let Some(store) = app.store() else {
                app.vault_busy = false;
                app.status = "Workspace storage is unavailable.".to_string();
                return Task::none();
            };
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        reencrypt_all_secrets(&store, master.as_bytes(), crate::VAULT_DEFAULT_KEY)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                Message::VaultDisableResult,
            )
        }
        Message::VaultDisableResult(result) => {
            app.vault_busy = false;
            match result {
                Ok(()) => {
                    app.vault_enabled = false;
                    app.vault_master = None;
                    app.vault_has_canary = false;
                    if let Some(store) = app.store() {
                        let _ = store.delete_master_canary();
                    }
                    app.persist_settings();
                    app.status = "Credential vault disabled.".to_string();
                }
                Err(reason) => {
                    app.status = format!("Could not disable vault: {reason}");
                }
            }
            Task::none()
        }
    }
}

/// Auto-lock the vault after this much inactivity.
const VAULT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Plaintext sentinel encrypted under the master password to verify it on unlock.
const VAULT_CANARY_PLAINTEXT: &[u8] = b"OpenTerm vault v1";

/// Handle vault setup (first run) or unlock, running Argon2id off the UI thread.
fn vault_submit(app: &mut App) -> Task<Message> {
    let pw = app.vault_pw.clone();
    let Some(store) = app.store() else {
        app.vault_err = Some("Workspace storage is unavailable.".to_string());
        return Task::none();
    };

    if app.vault_has_canary {
        // Unlock: derive key, try to decrypt the stored canary.
        if pw.is_empty() {
            app.vault_err = Some("Enter your master password.".to_string());
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let canary = store
                        .get_master_canary()
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "No master password set.".to_string())?;
                    let vault = openterm_crypto::LocalVault::new(openterm_crypto::VaultConfig::default());
                    match vault.decrypt_secret(pw.as_bytes(), &canary) {
                        Ok(plain) if plain == VAULT_CANARY_PLAINTEXT => Ok(pw),
                        _ => Err("Incorrect master password.".to_string()),
                    }
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            Message::VaultUnlockResult,
        )
    } else {
        // First-time setup: validate inputs, then encrypt & store the canary.
        let confirm = app.vault_confirm.clone();
        if pw.len() < 8 {
            app.vault_err = Some("Master password must be at least 8 characters.".to_string());
            return Task::none();
        }
        if pw != confirm {
            app.vault_err = Some("Passwords do not match.".to_string());
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let vault = openterm_crypto::LocalVault::new(openterm_crypto::VaultConfig::default());
                    let canary = vault
                        .encrypt_secret(pw.as_bytes(), VAULT_CANARY_PLAINTEXT)
                        .map_err(|e| e.to_string())?;
                    store.set_master_canary(&canary).map_err(|e| e.to_string())?;
                    Ok(pw)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            Message::VaultSetupResult,
        )
    }
}

/// Re-encrypt every saved secret from `old_key` to `new_key`, used when the
/// vault is enabled (default→master) or disabled (master→default). All secrets
/// are decrypted and re-encrypted in memory first; only if every one succeeds
/// are they written back in a single transaction, so a wrong key or a mid-way
/// failure never leaves the store half-converted. Each secret keeps its `id`
/// so host references stay valid.
fn reencrypt_all_secrets(
    store: &openterm_storage::WorkspaceStore,
    old_key: &[u8],
    new_key: &[u8],
) -> Result<(), String> {
    let vault = openterm_crypto::LocalVault::new(openterm_crypto::VaultConfig::default());
    let secrets = store.list_secrets().map_err(|e| e.to_string())?;
    let mut migrated = Vec::with_capacity(secrets.len());
    for secret in &secrets {
        let plain = vault
            .decrypt_secret(old_key, secret)
            .map_err(|_| "Could not decrypt an existing credential.".to_string())?;
        let mut re = vault
            .encrypt_secret(new_key, &plain)
            .map_err(|e| e.to_string())?;
        // Preserve the original id so AuthRef references in hosts stay valid.
        re.id = secret.id;
        migrated.push(re);
    }
    store.put_secrets_batch(&migrated).map_err(|e| e.to_string())?;
    Ok(())
}

/// Handle a chunk of bytes the user typed into the terminal. Beyond forwarding
/// to the PTY, this is where Sprint 3 smart-input lives: accepting an inline
/// ghost suggestion (Right/Tab), expanding a snippet abbreviation (Space/Tab),
/// and recomputing the next suggestion from history.
fn terminal_input(app: &mut App, bytes: Vec<u8>) -> Task<Message> {
    // 1) Snippet expansion: when the user presses Space or Tab and the whole
    //    typed line equals a known abbreviation, erase the abbr and send the
    //    expansion followed by the original trigger byte.
    let is_space = bytes == b" ";
    let is_tab = bytes == [0x09];
    if is_space || is_tab {
        let line = app
            .active_session()
            .map(|s| s.input_line())
            .unwrap_or_default();
        if let Some(expansion) = app
            .snippets
            .iter()
            .find(|s| s.abbr == line && !line.is_empty())
            .map(|s| s.expansion.clone())
        {
            let abbr_chars = line.chars().count();
            let mut out: Vec<u8> = vec![0x7f; abbr_chars]; // backspace ×N
            out.extend_from_slice(expansion.as_bytes());
            out.extend_from_slice(&bytes); // original Space/Tab
            if let Some(session) = app.active_session_mut() {
                session.terminal.scroll_to_bottom();
                // Resync the shadow: abbr erased, expansion typed. A trailing
                // Space is part of the line; a Tab is a completion trigger we
                // don't keep in the shadow.
                session.clear_input_line();
                session.extend_input(expansion.as_bytes());
                if is_space {
                    session.extend_input(b" ");
                }
                session.inline_suggestion = None;
            }
            if let Some(session) = app.active_session() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Write(out));
                }
            }
            recompute_active_suggestion(app);
            return Task::none();
        }
    }

    // 2) Accept the inline ghost suggestion: Right-arrow or Tab when one exists.
    let is_right = bytes == b"\x1b[C";
    if (is_right || is_tab) && app.active_session().is_some_and(|s| s.inline_suggestion.is_some()) {
        let suffix = app
            .active_session_mut()
            .and_then(|s| s.inline_suggestion.take());
        if let Some(suffix) = suffix {
            if let Some(session) = app.active_session_mut() {
                session.terminal.scroll_to_bottom();
                session.extend_input(suffix.as_bytes());
            }
            if let Some(session) = app.active_session() {
                if let Some(tx) = &session.cmd_tx {
                    let _ = tx.try_send(Command::Write(suffix.into_bytes()));
                }
            }
            recompute_active_suggestion(app);
            return Task::none();
        }
    }

    // 3) Normal path: track the bytes and forward them to the PTY.
    if let Some(session) = app.active_session_mut() {
        session.terminal.scroll_to_bottom();
        session.track_input(&bytes);
        // Enter clears the line; drop any stale suggestion immediately so it
        // doesn't flash on the next prompt.
        if bytes.iter().any(|&b| b == b'\r' || b == b'\n') {
            session.inline_suggestion = None;
        }
    }
    if let Some(session) = app.active_session() {
        if let Some(tx) = &session.cmd_tx {
            let _ = tx.try_send(Command::Write(bytes));
        }
    }
    recompute_active_suggestion(app);
    Task::none()
}

/// Recompute the active session's ghost suggestion.
///
/// Two-phase logic:
/// 1. **Command-name phase** (no space yet): suggest from this session's own
///    command history (e.g. "gi" → "t push origin main"). Never uses global
///    history — that would pollute across servers.
/// 2. **Argument phase** (space after command): use the smart-suggestion engine.
///    Picks a [`SuggestStrategy`] for the command, checks the session's cache,
///    and either produces a suggestion from cached data or triggers an async
///    remote query (which will call us back via `ConnEvent::SuggestionData`).
///
/// Anti-flicker: if the user's last keystroke extends the existing suggestion,
/// we trim instead of recomputing from scratch.
fn recompute_active_suggestion(app: &mut App) {
    let Some(index) = app.sessions.get(app.active).map(|_| app.active) else {
        return;
    };
    let line = app.sessions[index].input_line();

    // Stabilization: if we already have a suggestion and the user just typed
    // along its path, trim instead of full recompute.
    //
    // Operate on `char`s, never byte offsets: the input line can end in a
    // multi-byte UTF-8 char (e.g. `cd 文档`), and slicing at `len()-1` would
    // split a codepoint and panic the whole app.
    if let Some(existing) = &app.sessions[index].inline_suggestion {
        if let (Some(typed_char), Some(suggest_first)) =
            (line.chars().next_back(), existing.chars().next())
        {
            if typed_char == suggest_first {
                let trimmed: String = existing.chars().skip(1).collect();
                app.sessions[index].inline_suggestion =
                    if trimmed.is_empty() { None } else { Some(trimmed) };
                return;
            }
        }
    }

    // Only complete the command actually being typed right now: the tail
    // segment after any `|`/`;`/`&` (so `ps -ef | gr` completes `grep`, not
    // garbage appended to `ps`).
    let segment = crate::session::last_segment(&line);
    if segment.is_empty() {
        app.sessions[index].inline_suggestion = None;
        return;
    }
    let (cmd, arg) = crate::session::split_command_and_arg(segment);

    // Phase 1: command name (no space yet). History (full remembered command)
    // wins, then learned command names ranked by frecency, then a static list
    // of common command names for cold start.
    if arg.is_empty() {
        let suffix = {
            let session_hist = app.sessions[index]
                .command_history
                .iter()
                .rev()
                .map(String::as_str);
            crate::session::suggestion_suffix(segment, session_hist).or_else(|| {
                let learned = app.sessions[index].token_model.command_candidates(segment);
                crate::session::match_suffix(segment, learned.iter().map(String::as_str))
                    .or_else(|| {
                        let empty: [&str; 0] = [];
                        crate::session::command_name_suggestion(segment, empty.iter().copied())
                    })
            })
        };
        app.sessions[index].inline_suggestion = suffix;
        return;
    }

    // The token currently being typed (after the last space in `arg`) is what
    // completion should match against, not the whole argument string — this
    // is what makes multi-flag/multi-arg commands like `ps -ef`, `java -jar
    // app`, or `kill -9 123` complete correctly instead of never matching.
    let cur_token = crate::session::last_token(arg);

    // Flags: any token starting with `-`. Merge, in priority order:
    //   1. learned flags for this command (frecency-ranked, personalized),
    //   2. the static curated flag table,
    //   3. flags scraped from a cached `<cmd> --help`.
    // First extending match wins. On a total miss for an unknown command,
    // fire a one-shot `--help` scrape so future keystrokes have data.
    if cur_token.starts_with('-') {
        let mut candidates: Vec<String> =
            app.sessions[index].token_model.token_candidates(&cmd, cur_token);
        for f in crate::session::flags_for(&cmd) {
            if !candidates.iter().any(|c| c == f) {
                candidates.push((*f).to_string());
            }
        }
        let help_key = format!("help:{cmd}");
        if let Some((cached, fetched_at)) =
            app.sessions[index].suggestion_state.remote_caches.get(&help_key)
        {
            if fetched_at.elapsed().as_millis() < 1_800_000 {
                for f in cached {
                    if !candidates.iter().any(|c| c == f) {
                        candidates.push(f.clone());
                    }
                }
            }
        }
        let suffix = crate::session::match_suffix(cur_token, candidates.iter().map(String::as_str));
        if suffix.is_some() {
            app.sessions[index].inline_suggestion = suffix;
            return;
        }
        // Nothing matched. If this command isn't in the static table, has been
        // run before (safe to probe), and we haven't scraped it yet, kick off
        // a `<cmd> --help` scrape. Otherwise just clear.
        maybe_scrape_help(app, index, &cmd);
        app.sessions[index].inline_suggestion = None;
        return;
    }

    // Phase 2: argument phase → smart suggestion engine.
    let strategy = crate::session::strategy_for(&cmd);

    // Subcommands: zero-network, instant.
    if let crate::session::SuggestStrategy::Subcommands(subs) = &strategy {
        let suffix = crate::session::match_suffix(cur_token, subs.iter().copied());
        app.sessions[index].inline_suggestion = suffix;
        return;
    }

    // Strategies that need cached data or a remote query.
    let tag = crate::session::strategy_tag(&strategy);
    let (query_cmd, ttl_ms) = crate::session::strategy_query(&strategy)
        .unwrap_or(("", 10_000));

    // Check cache freshness and produce a suggestion if data is available.
    let cached_suffix = match &strategy {
        crate::session::SuggestStrategy::Files => {
            if let Some(cache) = &app.sessions[index].suggestion_state.dir_cache {
                if cache.is_fresh(ttl_ms) {
                    crate::session::match_suffix(cur_token, cache.all_entries.iter().map(String::as_str))
                } else {
                    None
                }
            } else {
                None
            }
        }
        crate::session::SuggestStrategy::FilesWithExt(exts) => {
            if let Some(cache) = &app.sessions[index].suggestion_state.dir_cache {
                if cache.is_fresh(ttl_ms) {
                    let filtered = crate::session::filter_by_ext(&cache.all_entries, exts);
                    crate::session::match_suffix(cur_token, filtered.into_iter())
                } else {
                    None
                }
            } else {
                None
            }
        }
        crate::session::SuggestStrategy::Directories => {
            if let Some(cache) = &app.sessions[index].suggestion_state.dir_cache {
                if cache.is_fresh(ttl_ms) {
                    crate::session::match_suffix(cur_token, cache.dirs.iter().map(String::as_str))
                } else {
                    None
                }
            } else {
                None
            }
        }
        crate::session::SuggestStrategy::ProcessList => {
            if let Some((pids, fetched_at)) = &app.sessions[index].suggestion_state.pid_cache {
                if fetched_at.elapsed().as_millis() < ttl_ms as u128 {
                    crate::session::match_suffix(cur_token, pids.iter().map(String::as_str))
                } else {
                    None
                }
            } else {
                None
            }
        }
        crate::session::SuggestStrategy::Remote { .. } => {
            let key = cmd.clone();
            if let Some((cands, fetched_at)) =
                app.sessions[index].suggestion_state.remote_caches.get(&key)
            {
                if fetched_at.elapsed().as_millis() < ttl_ms as u128 {
                    crate::session::match_suffix(cur_token, cands.iter().map(String::as_str))
                } else {
                    None
                }
            } else {
                None
            }
        }
        crate::session::SuggestStrategy::Subcommands(_) => unreachable!(),
    };

    if let Some(suffix) = cached_suffix {
        app.sessions[index].inline_suggestion = Some(suffix);
        return;
    }

    // Live data missed. Fall back to the learned model: what value usually
    // follows the previous token (e.g. `prod` after `--deploy-env`), then any
    // learned token for this command. This is the main win for third-party
    // commands whose values the static strategies know nothing about.
    let prev = crate::session::prev_token(arg);
    let learned = {
        let model = &app.sessions[index].token_model;
        let mut c = model.bigram_candidates(&cmd, prev, cur_token);
        for t in model.token_candidates(&cmd, cur_token) {
            if !c.iter().any(|x| *x == t) {
                c.push(t);
            }
        }
        c
    };
    if let Some(suffix) =
        crate::session::match_suffix(cur_token, learned.iter().map(String::as_str))
    {
        app.sessions[index].inline_suggestion = Some(suffix);
        return;
    }

    // Cache miss: trigger a remote query (unless one is already in flight).
    let tag_string = if tag.is_empty() { cmd.clone() } else { tag.to_string() };
    if !app.sessions[index].suggestion_state.is_pending(&tag_string) {
        app.sessions[index].suggestion_state.mark_pending(&tag_string);
        if let Some(tx) = &app.sessions[index].cmd_tx {
            let _ = tx.try_send(Command::ExecQuery {
                command: query_cmd.to_string(),
                tag: tag_string,
            });
        }
    }
    // While the query is in flight, clear the suggestion (no flicker — the
    // result will arrive and fill it in ~200ms).
    app.sessions[index].inline_suggestion = None;
}

/// Fire a one-shot `<cmd> --help` scrape to discover flags for a command not
/// covered by the static table. Gated hard for safety: the command must be a
/// plain word-like name, must NOT be in the static flag table (those are
/// already covered), must have actually been run before in this session's
/// history (so we never execute an unknown/typo'd binary speculatively), and
/// must not already be cached or in flight. Results land in `remote_caches`
/// under `help:<cmd>` via the normal `SuggestionData` path.
fn maybe_scrape_help(app: &mut App, index: usize, cmd: &str) {
    if !crate::session::is_help_probe_safe(cmd) {
        return;
    }
    if !crate::session::flags_for(cmd).is_empty() {
        return; // static table already covers it
    }
    let help_key = format!("help:{cmd}");
    let state = &app.sessions[index].suggestion_state;
    if state.remote_caches.contains_key(&help_key) || state.is_pending(&help_key) {
        return;
    }
    // Only probe commands the user has actually run (avoid executing arbitrary
    // typed-but-never-run strings).
    let ran_before = app.sessions[index]
        .command_history
        .iter()
        .any(|c| c.split_whitespace().next() == Some(cmd));
    if !ran_before {
        return;
    }
    app.sessions[index].suggestion_state.mark_pending(&help_key);
    if let Some(tx) = &app.sessions[index].cmd_tx {
        // `2>&1` so tools that print help to stderr are still captured; keep it
        // bounded so a misbehaving command can't flood us.
        let _ = tx.try_send(Command::ExecQuery {
            command: format!("{cmd} --help 2>&1 | head -200"),
            tag: help_key,
        });
    }
}

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
            // Clear any leftover terminal output from a prior connection so the
            // view switches back to the connect/edit form. Without this, a
            // disconnected session keeps its scrollback and `terminal_has_content()`
            // stays true, so the edit panel never shows (the user had to close the
            // whole tab to see it).
            session.clear_grid();
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
        app.closing_tabs.clear();
        app.spawn_session(config, cols, rows);
        return Task::none();
    }
    if index >= app.sessions.len() {
        return Task::none();
    }
    let id = app.sessions[index].id;
    // Already animating out — ignore repeat close requests.
    if app.closing_tabs.contains_key(&id) {
        return Task::none();
    }
    // Tell the worker to disconnect now; the session is dropped once the
    // collapse animation finishes (see `finalize_tab_close`).
    if let Some(tx) = &app.sessions[index].cmd_tx {
        let _ = tx.try_send(Command::Disconnect);
    }
    // If the active tab is the one closing, move focus to a neighbor that is
    // not itself animating out, so the workspace immediately shows a live tab.
    if app.active == index {
        app.active = if index > 0 {
            index - 1
        } else {
            (index + 1..app.sessions.len())
                .find(|i| !app.closing_tabs.contains_key(&app.sessions[*i].id))
                .unwrap_or(index)
        };
    }
    // Start the collapse animation (width 1.0 → 0.0), keyed by stable id.
    let now = std::time::Instant::now();
    app.now = now;
    let mut anim = Animation::new(true).quick();
    anim.go_mut(false, now);
    app.closing_tabs.insert(id, anim);
    Task::none()
}

/// Remove a session whose tab-close animation has finished, fixing up `active`.
fn finalize_tab_close(app: &mut App, id: u64) {
    let Some(index) = app.session_index_by_id(id) else {
        return;
    };
    app.sessions.remove(index);
    if app.sessions.is_empty() {
        // Never leave the workspace empty (shouldn't happen — we keep ≥1).
        let config = app.blank_config();
        let (cols, rows) = app.current_grid();
        app.spawn_session(config, cols, rows);
        return;
    }
    if app.active >= app.sessions.len() {
        app.active = app.sessions.len() - 1;
    } else if index < app.active {
        app.active -= 1;
    }
}

/// Re-derive the grid from the current window and push a resize to every
/// connected session.
fn apply_grid(app: &mut App) -> Task<Message> {
    let (cols, rows) = app.current_grid();
    for session in &mut app.sessions {
        // Only touch the PTY when the size actually changes. `apply_grid` now
        // runs every animation frame (via `Tick`), and `any_animating()` stays
        // true for seconds while a toast is up or a copy-flash fades, so an
        // unconditional resize would spam the PTY channel with no-op resizes.
        let changed = cols != session.grid_cols || rows != session.grid_rows;
        session.resize_grid(cols, rows);
        if changed && session.phase.is_active() {
            if let Some(tx) = &session.cmd_tx {
                let _ = tx.try_send(Command::Resize { cols, rows });
            }
        }
    }
    Task::none()
}

fn sftp_refresh(app: &mut App) -> Task<Message> {
    if let Some(session) = app.active_session_mut() {
        if session.sftp_open && session.phase == Phase::Connected {
            if let Some(tx) = &session.cmd_tx {
                let _ = tx.try_send(Command::SftpList(session.remote_path.clone()));
                // Immediate feedback: mid-transfer a listing can take a while
                // (the link is saturated), so show the click registered.
                session.sftp_status = format!("Loading {} …", session.remote_path);
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

/// Send a pause/cancel command for a transfer to the active session's worker.
fn transfer_control(app: &mut App, _id: u64, command: crate::connection::Command) {
    if let Some(session) = app.active_session() {
        if let Some(tx) = &session.cmd_tx {
            let _ = tx.try_send(command);
        }
    }
}

/// Resume a paused transfer by re-issuing the original download/upload command
/// with the same id. The `.part` scratch on disk drives the resume offset, so
/// the worker picks up where it left off.
fn resume_transfer(app: &mut App, id: u64) {
    let Some(session) = app.active_session_mut() else {
        return;
    };
    let Some(t) = session.transfers.iter_mut().find(|t| t.id == id) else {
        return;
    };
    if t.status != crate::session::TransferStatus::Paused {
        return;
    }
    // Flip the row back to Active; the worker will emit fresh progress.
    t.status = crate::session::TransferStatus::Active;
    t.finished_at = None;
    t.speed_bps = 0.0;
    t.pause_requested = false;
    let (direction, name, remote, local, size, is_dir) = (
        t.direction,
        t.name.clone(),
        t.remote.clone(),
        t.local.clone(),
        t.total,
        t.is_dir,
    );
    if let Some(tx) = &session.cmd_tx {
        let command = match direction {
            crate::connection::Direction::Download => crate::connection::Command::SftpDownload {
                id,
                name,
                remote,
                local,
                // Total is already known; pass it so the bar keeps its total.
                // The `.part` on disk is what actually drives the resume offset.
                size,
                is_dir,
            },
            crate::connection::Direction::Upload => crate::connection::Command::SftpUpload {
                id,
                name,
                local,
                remote,
                size,
                is_dir,
            },
        };
        let _ = tx.try_send(command);
    }
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
    // Async storage tasks accumulated by the Output branch (never blocking UI).
    let mut extra_tasks: Vec<Task<Message>> = vec![];
    let mut open_sftp_after = false;
    let mut open_menu_after = false;
    let mut download_after: Option<usize> = None;
    let mut touch_host_id = None;
    // Toast to surface after the `session` borrow ends (kind, message).
    let mut toast: Option<(crate::ui::toasts::ToastKind, String)> = None;
    let mut recompute_after = false;
    // When true, re-derive the grid once the `session` borrow ends. Set on the
    // Connected transition: going Connected makes the `Terminal | Files` subtab
    // bar appear, so `reserved_top` jumps 0→SUBTAB_HEIGHT and the terminal
    // canvas shrinks. Without a re-grid the grid keeps its taller pre-connect
    // row count, so the bottom rows (and the cursor) fall below the canvas and
    // get clipped — the terminal looks like it has no cursor and won't accept
    // input until the next window resize.
    let mut regrid_after = false;
    // Host label whose persisted history should seed the learned token model
    // once the `session` borrow below is released (set on Connected).
    let mut seed_model_host: Option<String> = None;
    // Field-level clone (not the `store()` method): `session` below holds a
    // mutable borrow of `app.sessions`, and a method call would borrow all of
    // `app`. Disjoint field access keeps the borrow checker happy.
    let store_handle = app.store.clone();
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
            session.connected_at = Some(std::time::Instant::now());
            // The subtab bar now appears (SSH sessions), shrinking the canvas;
            // re-derive the grid once the borrow ends so nothing is clipped.
            regrid_after = true;
            crate::smoke::record(&smoke, "connected");
            touch_host_id = session.config.host_id;
            // Seed the learned token model from this host's persisted history
            // (deferred until the `session` borrow ends, below).
            seed_model_host = Some(session.config.target_label());
            toast = Some((
                crate::ui::toasts::ToastKind::Success,
                format!("Connected to {}", session.config.target_label()),
            ));
        }
        ConnEvent::Output { bytes, .. } => {
            let prev_len = session.command_history.len();
            session.write_output(&bytes);
            // Answer what the emulator queued while parsing this chunk:
            // query responses (cursor position, DECRQM, DA — TUIs probe these
            // on startup) go back to the PTY; OSC 52 stores (how opencode/
            // tmux/nvim "copy" over SSH) go to the system clipboard.
            for ev in session.terminal.take_events() {
                match ev {
                    openterm_terminal::TerminalEvent::PtyWrite(reply) => {
                        if let Some(tx) = &session.cmd_tx {
                            let _ = tx.try_send(Command::Write(reply));
                        }
                    }
                    openterm_terminal::TerminalEvent::ClipboardStore(text) => {
                        let chars = text.chars().count();
                        toast = Some((
                            crate::ui::toasts::ToastKind::Success,
                            format!("Copied {chars} chars (remote)"),
                        ));
                        extra_tasks.push(clipboard::write(text));
                    }
                }
            }
            let new_entry = if session.command_history.len() > prev_len {
                // Backfill the previous in-memory entry with its now-captured output.
                let committed = std::mem::take(&mut session.committed_output);
                if !committed.is_empty() {
                    if let Some(prev) = app.all_history.front_mut() {
                        prev.output.clone_from(&committed);
                    }
                    // #23: async write — never blocks the UI thread.
                    if let Some(store) = store_handle.clone() {
                        extra_tasks.push(Task::perform(
                            async move {
                                let _ = tokio::task::spawn_blocking(move || {
                                    let _ = store.update_last_history_output(&committed);
                                }).await;
                            },
                            |_| Message::Noop,
                        ));
                    }
                }
                session.command_history.last().map(|cmd: &String| {
                    // Live-learn the just-committed command into the token model.
                    session.token_model.learn(cmd);
                    openterm_storage::HistoryEntry {
                        ts_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        host: session.config.target_label(),
                        cmd: cmd.clone(),
                        output: String::new(),
                    }
                })
            } else {
                None
            };
            if let Some(entry) = new_entry {
                // #23: async write — never blocks the UI thread.
                if let Some(store) = store_handle.clone() {
                    let e2 = entry.clone();
                    extra_tasks.push(Task::perform(
                        async move {
                            let _ = tokio::task::spawn_blocking(move || {
                                let _ = store.append_history(&e2);
                            }).await;
                        },
                        |_| Message::Noop,
                    ));
                }
                app.all_history.push_front(entry); // #10: O(1) push_front
            }
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
            remote,
            local,
            is_dir,
            ..
        } => {
            crate::smoke::record(&smoke, "transfer_started");
            // A resume re-issues TransferStarted with the same id: update the
            // existing row in place rather than inserting a duplicate.
            if let Some(t) = session.transfers.iter_mut().find(|t| t.id == id) {
                t.status = crate::session::TransferStatus::Active;
                t.finished_at = None;
                if total > 0 {
                    t.total = total;
                }
            } else {
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
                        started_at: std::time::Instant::now(),
                        finished_at: None,
                        remote,
                        local,
                        is_dir,
                        pause_requested: false,
                    },
                );
                // Cap history length.
                session.transfers.truncate(40);
            }
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
                        t.finished_at = Some(std::time::Instant::now());
                    }
                    Err(e) => {
                        t.status = crate::session::TransferStatus::Failed(e);
                        t.finished_at = Some(std::time::Instant::now());
                    }
                }
            }
            // A finished transfer changes both panes (new file present).
            session.refresh_local(sort, sort_asc);
            return sftp_refresh(app);
        }
        ConnEvent::TransferPaused { id, transferred, .. } => {
            if let Some(t) = session.transfers.iter_mut().find(|t| t.id == id) {
                t.transferred = transferred;
                t.speed_bps = 0.0;
                t.status = crate::session::TransferStatus::Paused;
                t.pause_requested = false;
                t.finished_at = Some(std::time::Instant::now());
            }
        }
        ConnEvent::TransferCanceled { id, .. } => {
            // Drop the row entirely — the `.part` scratch was removed worker-side.
            session.transfers.retain(|t| t.id != id);
            // A cancelled upload may have left/removed a remote `.part`; refresh.
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
        ConnEvent::Ports { raw, .. } => {
            session.ports = crate::metrics::parse_ports(&raw);
        }
        ConnEvent::Exit { code, .. } => {
            session.status = format!("Process exited ({code})");
        }
        ConnEvent::FileChunk { path, offset, data, total, .. } => {
            if let Some(fv) = &mut session.file_viewer {
                if fv.path == path {
                    let text = String::from_utf8_lossy(&data).into_owned();
                    // Compute once here; view() reads the cache, never calls highlight().
                    fv.highlight_cache = crate::highlight::highlight(&text, &fv.lang);
                    if total <= crate::session::FileViewerState::SMALL_FILE_LIMIT {
                        fv.content = crate::session::ViewerContent::Loaded(text);
                    } else {
                        fv.mode = crate::session::ViewerMode::Log;
                        fv.content = crate::session::ViewerContent::Streaming {
                            text,
                            total,
                            page_offset: offset,
                        };
                    }
                    fv.refresh_matches();
                }
            }
        }
        ConnEvent::FileSaved { result, .. } => {
            if let Some(fv) = &mut session.file_viewer {
                fv.saving = false;
                if result.is_ok() { fv.dirty = false; }
            }
        }
        ConnEvent::Closed { .. } => {
            session.phase = Phase::Idle;
            session.status = "Disconnected".to_string();
            session.monitor_panel = None;
            session.processes.clear();
            session.connected_at = None;
            session.suggestion_state.clear_all();
            session.inline_suggestion = None;
            // Leaving Connected hides the subtab band + rail, so the canvas
            // grows again; re-derive the grid to reclaim the rows.
            regrid_after = true;
            toast = Some((
                crate::ui::toasts::ToastKind::Info,
                format!("Disconnected from {}", session.config.target_label()),
            ));
        }
        ConnEvent::Failed { error, .. } => {
            crate::smoke::record(&smoke, "failed");
            session.phase = Phase::Failed(error.clone());
            session.status = error.clone();
            session.connected_at = None;
            session.suggestion_state.clear_all();
            session.inline_suggestion = None;
            // Same as Closed: the subtab band/rail go away, so re-grid.
            regrid_after = true;
            toast = Some((crate::ui::toasts::ToastKind::Error, error));
        }
        ConnEvent::SuggestionData { tag, candidates, .. } => {
            // Store the query result into the session's suggestion cache.
            session.suggestion_state.clear_pending(&tag);
            match tag.as_str() {
                "__files__" => {
                    let dirs: Vec<String> = candidates
                        .iter()
                        .filter(|c| c.ends_with('/'))
                        .cloned()
                        .collect();
                    session.suggestion_state.dir_cache = Some(
                        crate::session::DirCache {
                            all_entries: candidates,
                            dirs,
                            fetched_at: std::time::Instant::now(),
                        },
                    );
                }
                "kill" | "pkill" | "killall" => {
                    session.suggestion_state.pid_cache =
                        Some((candidates, std::time::Instant::now()));
                }
                "cd" | "pushd" => {
                    // Directories-only query result → store in dir_cache as well
                    // (only the dirs field is populated for cd-strategy).
                    let dirs = candidates.clone();
                    let all = dirs.clone();
                    session.suggestion_state.dir_cache = Some(
                        crate::session::DirCache {
                            all_entries: all,
                            dirs,
                            fetched_at: std::time::Instant::now(),
                        },
                    );
                }
                _ => {
                    // Custom Remote-strategy cache.
                    session.suggestion_state.remote_caches.insert(
                        tag,
                        (candidates, std::time::Instant::now()),
                    );
                }
            }
            // Mark that we need to recompute the suggestion after the borrow.
            recompute_after = true;
        }
    }
    // Borrow of `session` ends here; safe to touch `app` again.
    if let Some(host) = seed_model_host {
        // Feed this host's persisted history (oldest-first) into the freshly
        // connected session's token model, so learning carries across runs.
        let commands: Vec<String> = app
            .all_history
            .iter()
            .rev() // all_history is newest-first; reverse → oldest-first
            .filter(|e| e.host == host)
            .map(|e| e.cmd.clone())
            .collect();
        if let Some(session) = app.sessions.get_mut(index) {
            session.seed_token_model(commands.iter().map(String::as_str));
        }
    }
    if recompute_after {
        recompute_active_suggestion(app);
    }
    if regrid_after {
        // Resize the grid + push a PTY resize now that the layout reserves the
        // subtab band. `resize_grid` no-ops when the size is unchanged, so this
        // is cheap when the grid was already correct.
        let _ = apply_grid(app);
    }
    if let Some((kind, msg)) = toast {
        app.push_toast(kind, msg);
    }
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
    if extra_tasks.is_empty() { Task::none() } else { Task::batch(extra_tasks) }
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

fn open_file_viewer(app: &mut App, path: String) -> Task<Message> {
    use crate::session::FileViewerState;
    let Some(session) = app.active_session_mut() else { return Task::none(); };
    session.file_viewer = Some(FileViewerState::new_loading(path.clone()));
    let Some(tx) = session.cmd_tx.clone() else { return Task::none(); };
    let _ = tx.try_send(Command::ReadFileRange {
        path,
        offset: 0,
        len: FileViewerState::SMALL_FILE_LIMIT,
    });
    Task::none()
}

/// `Message::FileViewerChunk` is never dispatched by the UI — file chunks arrive
/// via `Message::Conn(ConnEvent::FileChunk)` and are handled in `handle_conn_event`.
/// This stub satisfies the match arm.
#[allow(unused_variables)]
fn file_viewer_chunk(app: &mut App, offset: u64, data: Vec<u8>, total: u64) -> Task<Message> {
    Task::none()
}

fn file_viewer_save(app: &mut App) -> Task<Message> {
    let Some(session) = app.active_session_mut() else { return Task::none(); };
    let Some(fv) = session.file_viewer.as_mut() else { return Task::none(); };
    // In edit mode, the source of truth is the text_editor; otherwise use loaded content.
    let data = if fv.mode == crate::session::ViewerMode::Edit {
        fv.editor.text().into_bytes()
    } else {
        match &fv.content {
            crate::session::ViewerContent::Loaded(t) => t.as_bytes().to_vec(),
            _ => return Task::none(),
        }
    };
    let path = fv.path.clone();
    fv.saving = true;
    let Some(tx) = session.cmd_tx.clone() else { return Task::none(); };
    let _ = tx.try_send(Command::WriteFile { path, data });
    Task::none()
}

fn file_viewer_page(app: &mut App, next: bool) -> Task<Message> {    use crate::session::{FileViewerState, ViewerContent};
    let Some(session) = app.active_session_mut() else { return Task::none(); };
    let Some(fv) = session.file_viewer.as_mut() else { return Task::none(); };
    let (page_offset, total, path) = match &fv.content {
        ViewerContent::Streaming { page_offset, total, .. } => (*page_offset, *total, fv.path.clone()),
        _ => return Task::none(),
    };
    let new_offset = if next {
        (page_offset + FileViewerState::PAGE_SIZE).min(total.saturating_sub(FileViewerState::PAGE_SIZE))
    } else {
        page_offset.saturating_sub(FileViewerState::PAGE_SIZE)
    };
    if new_offset == page_offset { return Task::none(); }
    fv.content = ViewerContent::Loading;
    let Some(tx) = session.cmd_tx.clone() else { return Task::none(); };
    let _ = tx.try_send(Command::ReadFileRange { path, offset: new_offset, len: FileViewerState::PAGE_SIZE });
    Task::none()
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
