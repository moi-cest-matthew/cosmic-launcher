use crate::app::iced::event::listen_raw;
use crate::subscriptions::launcher;
use clap::Parser;
use cosmic::app::{Command, Core, CosmicFlags, DbusActivationDetails, Settings};
use cosmic::cctk::sctk;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::id::Id;
use cosmic::iced::wayland::actions::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::wayland::actions::popup::{SctkPopupSettings, SctkPositioner};
use cosmic::iced::wayland::layer_surface::{
    destroy_layer_surface, get_layer_surface, Anchor, KeyboardInteractivity,
};
use cosmic::iced::widget::{column, container, text, Column};
use cosmic::iced::{self, Length, Subscription};
use cosmic::iced_core::{Padding, Point, Rectangle};
use cosmic::iced_runtime::core::event::wayland::LayerEvent;
use cosmic::iced_runtime::core::event::{wayland, PlatformSpecific};
use cosmic::iced_runtime::core::layout::Limits;
use cosmic::iced_runtime::core::window::Id as SurfaceId;
use cosmic::iced_sctk::commands;
use cosmic::iced_sctk::commands::activation::request_token;
use cosmic::iced_style::{application, container::Appearance as ContainerAppearance};
use cosmic::iced_widget::row;
use cosmic::theme::{self, Button, Container};
use cosmic::widget::icon::{from_name, IconFallback};
use cosmic::widget::{button, divider, horizontal_space, icon, mouse_area, scrollable, text_input};
use cosmic::{keyboard_nav, Element, Theme};
use iced::keyboard::KeyCode;
use iced::wayland::Appearance;
use iced::widget::vertical_space;
use iced::{Alignment, Color};
use once_cell::sync::Lazy;
use pop_launcher::{ContextOption, IconSource, SearchResult};
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use tokio::sync::mpsc;

static INPUT_ID: Lazy<Id> = Lazy::new(|| Id::new("input_id"));
pub(crate) static WINDOW_ID: Lazy<SurfaceId> = Lazy::new(SurfaceId::unique);
pub(crate) static MENU_ID: Lazy<SurfaceId> = Lazy::new(SurfaceId::unique);

#[derive(Parser, Debug, Serialize, Deserialize, Clone)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Args {}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LauncherCommands;

impl ToString for LauncherCommands {
    fn to_string(&self) -> String {
        serde_json::ser::to_string(self).unwrap()
    }
}

impl CosmicFlags for Args {
    type SubCommand = LauncherCommands;
    type Args = Vec<String>;

    fn action(&self) -> Option<&LauncherCommands> {
        None
    }
}

pub fn run() -> cosmic::iced::Result {
    let args = Args::parse();
    cosmic::app::run_single_instance::<CosmicLauncher>(
        Settings::default()
            .antialiasing(true)
            .client_decorations(true)
            .debug(false)
            .default_text_size(16.0)
            .scale_factor(1.0)
            .no_main_window(true)
            .exit_on_close(false),
        args,
    )
}

pub fn menu_button<'a, Message>(
    content: impl Into<Element<'a, Message>>,
) -> cosmic::widget::Button<'a, Message, cosmic::Renderer> {
    cosmic::widget::Button::new(content)
        .style(Button::AppletMenu)
        .padding(menu_control_padding())
        .width(Length::Fill)
}

pub fn menu_control_padding() -> Padding {
    let theme = cosmic::theme::active();
    let cosmic = theme.cosmic();
    [cosmic.space_xxs(), cosmic.space_m()].into()
}

#[derive(Clone)]
pub struct CosmicLauncher {
    core: Core,
    input_value: String,
    active_surface: bool,
    launcher_items: Vec<SearchResult>,
    tx: Option<mpsc::Sender<launcher::Request>>,
    wait_for_result: bool,
    menu: Option<(u32, Vec<ContextOption>)>,
    cursor_position: Option<Point<f32>>,
}

#[derive(Debug, Clone)]
pub enum Message {
    InputChanged(String),
    Activate(usize),
    Context(usize),
    MenuButton(u32, u32),
    CloseContextMenu,
    CursorMoved(Point<f32>),
    Hide,
    LauncherEvent(launcher::Event),
    Layer(LayerEvent),
    KeyboardNav(keyboard_nav::Message),
    ActivationToken(Option<String>, String),
}

impl CosmicLauncher {
    fn hide(&mut self) -> Command<Message> {
        self.input_value.clear();

        // XXX The close will reset the launcher, but the search will restart it so it's ready
        // for the next time it's opened.
        if let Some(ref sender) = &self.tx {
            let _res = sender.blocking_send(launcher::Request::Close);
        }

        if let Some(tx) = &self.tx {
            let _res = tx.blocking_send(launcher::Request::Search(String::new()));
        } else {
            tracing::info!("NOT FOUND");
        }

        if self.active_surface {
            self.active_surface = false;
            
            let mut commands = vec![destroy_layer_surface(*WINDOW_ID)];
            if self.menu.take().is_some() {
                commands.push(commands::popup::destroy_popup(*MENU_ID));
            }
            return Command::batch(commands);
        }

        Command::none()
    }
}

impl cosmic::Application for CosmicLauncher {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = Args;
    const APP_ID: &'static str = "com.system76.CosmicLauncher";

    fn init(core: Core, _flags: Args) -> (Self, Command<Message>) {
        (
            CosmicLauncher {
                core,
                input_value: String::new(),
                active_surface: false,
                launcher_items: Vec::new(),
                tx: None,
                wait_for_result: false,
                menu: None,
                cursor_position: None,
            },
            Command::none(),
        )
    }

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn style(&self) -> Option<<Theme as application::StyleSheet>::Style> {
        Some(<Theme as application::StyleSheet>::Style::Custom(Box::new(
            |theme| Appearance {
                background_color: Color::from_rgba(0.0, 0.0, 0.0, 0.0),
                text_color: theme.cosmic().on_bg_color().into(),
                icon_color: theme.cosmic().on_bg_color().into(),
            },
        )))
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Message) -> Command<Self::Message> {
        match message {
            Message::InputChanged(value) => {
                self.input_value = value.clone();
                if let Some(tx) = &self.tx {
                    let _res = tx.blocking_send(launcher::Request::Search(value));
                }
            }
            Message::Activate(i) => {
                if let (Some(tx), Some(item)) = (&self.tx, self.launcher_items.get(i)) {
                    let _res = tx.blocking_send(launcher::Request::Activate(item.id));
                }
            }
            #[allow(clippy::cast_possible_wrap)]
            Message::Context(i) => {
                if self.menu.take().is_some() {
                    return commands::popup::destroy_popup(*MENU_ID);
                }

                if let (Some(tx), Some(item)) = (&self.tx, self.launcher_items.get(i)) {
                    let _res = tx.blocking_send(launcher::Request::Context(item.id));
                }
            }
            Message::CursorMoved(pos) => {
                self.cursor_position = Some(pos);
            }
            Message::MenuButton(i, context) => {
                if self.menu.take().is_some() {
                    return commands::popup::destroy_popup(*MENU_ID);
                }

                if let Some(tx) = &self.tx {
                    let _res = tx.blocking_send(launcher::Request::ActivateContext(i, context));
                }
            }
            Message::LauncherEvent(e) => match e {
                launcher::Event::Started(tx) => {
                    _ = tx.blocking_send(launcher::Request::Search(String::new()));
                    self.tx.replace(tx);
                }
                launcher::Event::Response(response) => match response {
                    pop_launcher::Response::Close => return self.hide(),
                    #[allow(clippy::cast_possible_truncation)]
                    pop_launcher::Response::Context { id, options } => {
                        if options.is_empty() {
                            return Command::none();
                        }

                        self.menu = Some((id, options));
                        let Some(pos) = self.cursor_position.as_ref() else {
                            return Command::none()
                        };
                        let rect = Rectangle {
                            x: pos.x.round() as i32,
                            y: pos.y.round() as i32,
                            width: 1,
                            height: 1,
                        };

                        return commands::popup::get_popup(SctkPopupSettings {
                            parent: *WINDOW_ID,
                            id: *MENU_ID,
                            positioner: SctkPositioner {
                                size: None,
                                size_limits: Limits::NONE.min_width(1.0).min_height(1.0).max_width(300.0).max_height(800.0),
                                anchor_rect: rect,
                                anchor:
                                    sctk::reexports::protocols::xdg::shell::client::xdg_positioner::Anchor::Right,
                                gravity: sctk::reexports::protocols::xdg::shell::client::xdg_positioner::Gravity::Right,
                                reactive: true,
                                ..Default::default() 
                            },
                            grab: true,
                            parent_size: None,
                        });
                    }
                    pop_launcher::Response::DesktopEntry {
                        path,
                        gpu_preference: _,
                    } => {
                        if let Some(entry) = cosmic::desktop::load_desktop_file(None, path) {
                            let Some(exec) = entry.exec else {
                                return Command::none()
                            };

                            return request_token(
                                Some(String::from(Self::APP_ID)),
                                Some(*WINDOW_ID),
                                move |token| {
                                    cosmic::app::Message::App(Message::ActivationToken(
                                        token, exec,
                                    ))
                                },
                            );
                        }
                    }
                    pop_launcher::Response::Update(mut list) => {
                        list.sort_by(|a, b| {
                            let a = i32::from(a.window.is_none());
                            let b = i32::from(b.window.is_none());
                            a.cmp(&b)
                        });
                        list.truncate(10);
                        self.launcher_items.splice(.., list);

                        if self.wait_for_result {
                            self.wait_for_result = false;
                            return Command::batch(vec![get_layer_surface(
                                SctkLayerSurfaceSettings {
                                    id: *WINDOW_ID,
                                    keyboard_interactivity: KeyboardInteractivity::Exclusive,
                                    anchor: Anchor::TOP,
                                    namespace: "launcher".into(),
                                    size: None,
                                    margin: iced::wayland::actions::layer_surface::IcedMargin {
                                        top: 16,
                                        ..Default::default()
                                    },
                                    size_limits: Limits::NONE
                                        .min_width(1.0)
                                        .min_height(1.0)
                                        .max_width(600.0),
                                    ..Default::default()
                                },
                            )]);
                        }
                    }
                    pop_launcher::Response::Fill(s) => {
                        self.input_value = s;
                        return text_input::focus(INPUT_ID.clone());
                    }
                },
            },
            Message::Layer(e) => match e {
                LayerEvent::Focused => {
                    return text_input::focus(INPUT_ID.clone());
                }
                LayerEvent::Unfocused => {
                    return self.hide();
                }
                LayerEvent::Done => {}
            },
            Message::CloseContextMenu => if self.menu.take().is_some() {
                return commands::popup::destroy_popup(*MENU_ID);
            }
            Message::Hide => if self.menu.take().is_some() {
                return commands::popup::destroy_popup(*MENU_ID);
            } else {
                return self.hide()
            }
            Message::KeyboardNav(e) => {
                match e {
                    keyboard_nav::Message::FocusNext => {
                        return iced::widget::focus_next();
                    }
                    keyboard_nav::Message::FocusPrevious => {
                        return iced::widget::focus_previous();
                    }
                    keyboard_nav::Message::Unfocus => {
                        self.input_value.clear();
                        if let Some(tx) = &self.tx {
                            let _res = tx.blocking_send(launcher::Request::Search(String::new()));
                        }
                        return keyboard_nav::unfocus();
                    }
                    _ => {}
                };
            }
            Message::ActivationToken(token, exec) => {
                let mut envs = Vec::new();
                if let Some(token) = token {
                    envs.push(("XDG_ACTIVATION_TOKEN", token.clone()));
                    envs.push(("DESKTOP_STARTUP_ID", token));
                }
                cosmic::desktop::spawn_desktop_exec(exec, envs);
                return self.hide();
            }
        }
        Command::none()
    }

    fn dbus_activation(
        &mut self,
        msg: cosmic::app::DbusActivationMessage,
    ) -> iced::Command<cosmic::app::Message<Self::Message>> {
        if let DbusActivationDetails::Activate = msg.msg {
            if self.active_surface {
                self.hide()
            } else {
                if let Some(tx) = &self.tx {
                    let _res = tx.blocking_send(launcher::Request::Search(String::new()));
                } else {
                    tracing::info!("NOT FOUND");
                }

                self.input_value = String::new();
                self.active_surface = true;
                self.wait_for_result = true;
                Command::none()
            }
        } else {
            Command::none()
        }
    }

    fn view(&self) -> Element<Self::Message> {
        unimplemented!()
    }

    #[allow(clippy::too_many_lines)]
    fn view_window(&self, id: SurfaceId) -> Element<Self::Message> {
        if id == *WINDOW_ID {
            let launcher_entry = text_input::search_input(
                "Type to search apps or type “?” for more options...",
                &self.input_value,
            )
            .on_input(Message::InputChanged)
            .on_paste(Message::InputChanged)
            .on_submit(Message::Activate(0))
            .id(INPUT_ID.clone());

            let buttons: Vec<_> = self
                .launcher_items
                .iter()
                .enumerate()
                .flat_map(|(i, item)| {
                    let (name, desc) = if item.window.is_some() {
                        (&item.description, &item.name)
                    } else {
                        (&item.name, &item.description)
                    };

                    let name = Column::with_children(
                        name.lines()
                            .map(|line| {
                                text(if line.len() > 45 {
                                    format!("{line:.45}...")
                                } else {
                                    line.to_string()
                                })
                                .horizontal_alignment(Horizontal::Left)
                                .vertical_alignment(Vertical::Center)
                                .size(14)
                                .style(cosmic::theme::Text::Custom(|t| text::Appearance {
                                    color: Some(t.cosmic().on_bg_color().into()),
                                }))
                                .into()
                            })
                            .collect(),
                    );
                    let desc = Column::with_children(
                        desc.lines()
                            .map(|line| {
                                text(if line.len() > 60 {
                                    format!("{line:.60}")
                                } else {
                                    line.to_string()
                                })
                                .horizontal_alignment(Horizontal::Left)
                                .vertical_alignment(Vertical::Center)
                                .size(10)
                                .style(theme::Text::Custom(|t| text::Appearance {
                                    color: Some(t.cosmic().on_bg_color().into()),
                                }))
                                .into()
                            })
                            .collect(),
                    );

                    let mut button_content = Vec::new();
                    if let Some(source) = item.category_icon.as_ref() {
                        let name = match source {
                            IconSource::Name(name) | IconSource::Mime(name) => name,
                        };
                        button_content.push(
                            icon(from_name(name.clone()).into())
                                .width(Length::Fixed(16.0))
                                .height(Length::Fixed(16.0))
                                .style(cosmic::theme::Svg::Custom(Rc::new(|theme| {
                                    cosmic::iced_style::svg::Appearance {
                                        color: Some(theme.cosmic().on_bg_color().into()),
                                    }
                                })))
                                .into(),
                        );
                    }

                    if let Some(source) = item.icon.as_ref() {
                        let name = match source {
                            IconSource::Name(name) | IconSource::Mime(name) => name,
                        };
                        button_content.push(
                            icon(
                                from_name(name.clone())
                                    .size(64)
                                    .fallback(Some(IconFallback::Names(vec![
                                        "application-default".into(),
                                        "application-x-executable".into(),
                                    ])))
                                    .into(),
                            )
                            .width(Length::Fixed(32.0))
                            .height(Length::Fixed(32.0))
                            .into(),
                        );
                    }

                    button_content.push(column![name, desc].into());
                    button_content.push(
                        container(
                            text(format!("Ctrl + {}", (i + 1) % 10))
                                .size(14)
                                .vertical_alignment(Vertical::Center)
                                .horizontal_alignment(Horizontal::Right)
                                .style(theme::Text::Custom(|t| text::Appearance {
                                    color: Some(t.cosmic().on_bg_color().into()),
                                })),
                        )
                        .width(Length::Fill)
                        .center_y()
                        .align_y(Vertical::Center)
                        .align_x(Horizontal::Right)
                        .padding([8, 16])
                        .into(),
                    );

                    let btn = mouse_area(cosmic::widget::button(
                        row(button_content)
                            .spacing(8)
                            .align_items(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .on_press(Message::Activate(i))
                    .padding([8, 16])
                    .style(Button::Custom {
                        active: Box::new(|focused, theme| {
                            let rad_s = theme.cosmic().corner_radii.radius_s;
                            let text = button::StyleSheet::active(theme, focused, &Button::Text);
                            button::Appearance {
                                border_radius: rad_s.into(),
                                ..text
                            }
                        }),
                        hovered: Box::new(|focused, theme| {
                            let rad_s = theme.cosmic().corner_radii.radius_s;

                            let text = button::StyleSheet::hovered(theme, focused, &Button::Text);
                            button::Appearance {
                                border_radius: rad_s.into(),
                                ..text
                            }
                        }),
                        disabled: Box::new(|theme| {
                            let rad_s = theme.cosmic().corner_radii.radius_s;

                            let text = button::StyleSheet::disabled(theme, &Button::Text);
                            button::Appearance {
                                border_radius: rad_s.into(),
                                ..text
                            }
                        }),
                        pressed: Box::new(|focused, theme| {
                            let rad_s = theme.cosmic().corner_radii.radius_s;

                            let text = button::StyleSheet::pressed(theme, focused, &Button::Text);
                            button::Appearance {
                                border_radius: rad_s.into(),
                                ..text
                            }
                        }),
                    }))
                    .on_right_release(Message::Context(i));
                    if i == self.launcher_items.len() - 1 {
                        vec![btn.into()]
                    } else {
                        vec![btn.into(), divider::horizontal::light().into()]
                    }
                })
                .collect();

            let mut content = column![launcher_entry].max_width(600).spacing(16);

            if !buttons.is_empty() {
                content = content.push(column(buttons));
            }
            
            let window = container(content)
                .style(Container::Custom(Box::new(|theme| container::Appearance {
                    text_color: Some(theme.cosmic().on_bg_color().into()),
                    icon_color: Some(theme.cosmic().on_bg_color().into()),
                    background: Some(Color::from(theme.cosmic().background.base).into()),
                    border_radius: theme.cosmic().corner_radii.radius_m.into(),
                    border_width: 1.0,
                    border_color: theme.cosmic().bg_divider().into(),
                })))
                .padding([24, 32]);

            return if self.menu.is_some() {
                mouse_area(window)
                    .on_release(Message::CloseContextMenu)
                    .on_right_release(Message::CloseContextMenu)
                    .into()
            } else {
                window.into()
            };
        }

        if id == *MENU_ID {
            let Some((i, options)) = self
                .menu
                .as_ref()
            else {
                return container(horizontal_space(Length::Fixed(1.0)))
                    .width(Length::Fixed(1.0))
                    .height(Length::Fixed(1.0))
                    .into();
            };
            let list_column = Column::with_children(
                options.iter().map(|option| {
                    menu_button(text(&option.name)).on_press(Message::MenuButton(*i, option.id)).into()
                }).collect()
            )
                .padding([8, 0]);
            

            return container(container(scrollable(list_column)).style(
                theme::Container::custom(|theme| {
                    let cosmic = theme.cosmic();
                    let corners = cosmic.corner_radii;
                    ContainerAppearance {
                        text_color: Some(cosmic.background.on.into()),
                        background: Some(Color::from(cosmic.background.base).into()),
                        border_radius: corners.radius_m.into(),
                        border_width: 1.0,
                        border_color: cosmic.background.divider.into(),
                        icon_color: Some(cosmic.background.on.into()),
                    }
                }),
            ))
            .width(Length::Shrink)
            .height(Length::Shrink)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Top)
            .into();
        }
        
        vertical_space(Length::Fixed(1.0)).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch(
            vec![
                launcher::subscription(0).map(Message::LauncherEvent),
                listen_raw(|e, _status| match e {
                    cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                        wayland::Event::Layer(e, ..),
                    )) => Some(Message::Layer(e)),
                    cosmic::iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
                        key_code,
                        modifiers,
                    }) => match key_code {
                        KeyCode::Key1 | KeyCode::Numpad1 if modifiers.control() => {
                            Some(Message::Activate(0))
                        }
                        KeyCode::Key2 | KeyCode::Numpad2 if modifiers.control() => {
                            Some(Message::Activate(1))
                        }
                        KeyCode::Key3 | KeyCode::Numpad3 if modifiers.control() => {
                            Some(Message::Activate(2))
                        }
                        KeyCode::Key4 | KeyCode::Numpad4 if modifiers.control() => {
                            Some(Message::Activate(3))
                        }
                        KeyCode::Key5 | KeyCode::Numpad5 if modifiers.control() => {
                            Some(Message::Activate(4))
                        }
                        KeyCode::Key6 | KeyCode::Numpad6 if modifiers.control() => {
                            Some(Message::Activate(5))
                        }
                        KeyCode::Key7 | KeyCode::Numpad7 if modifiers.control() => {
                            Some(Message::Activate(6))
                        }
                        KeyCode::Key8 | KeyCode::Numpad7 if modifiers.control() => {
                            Some(Message::Activate(7))
                        }
                        KeyCode::Key9 | KeyCode::Numpad9 if modifiers.control() => {
                            Some(Message::Activate(8))
                        }
                        KeyCode::Key0 | KeyCode::Numpad0 if modifiers.control() => {
                            Some(Message::Activate(9))
                        }
                        KeyCode::Up => {
                            Some(Message::KeyboardNav(keyboard_nav::Message::FocusPrevious))
                        }
                        KeyCode::Down => {
                            Some(Message::KeyboardNav(keyboard_nav::Message::FocusNext))
                        }
                        KeyCode::P | KeyCode::K if modifiers.control() => {
                            Some(Message::KeyboardNav(keyboard_nav::Message::FocusPrevious))
                        }
                        KeyCode::N | KeyCode::J if modifiers.control() => {
                            Some(Message::KeyboardNav(keyboard_nav::Message::FocusNext))
                        }
                        KeyCode::Escape => Some(Message::Hide),
                        _ => None,
                    }
                    cosmic::iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => Some(Message::CursorMoved(position)),
                    _ => None,
                }),
            ],
        )
    }
}
