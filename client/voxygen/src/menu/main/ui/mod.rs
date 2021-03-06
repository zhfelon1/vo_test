mod connecting;
// Note: Keeping in case we re-add the disclaimer
//mod disclaimer;
mod credits;
mod login;
mod servers;

use crate::{
    credits::Credits,
    render::ThirdPassDrawer,
    ui::{
        self,
        fonts::IcedFonts as Fonts,
        ice::{load_font, style, widget, Element, IcedUi as Ui},
        img_ids::ImageGraphic,
        Graphic,
    },
    window, GlobalState,
};
use i18n::{LanguageMetadata, LocalizationHandle};
use iced::{Length, Horizontal};
use iced::widget::{Text, Column, Container, text_input, Row, Space};

use keyboard_keynames::key_layout::KeyLayout;
//ImageFrame, Tooltip,
use crate::settings::Settings;
use common::assets::{self, AssetExt};
use rand::{seq::SliceRandom, thread_rng};
use instant::Duration;

// TODO: what is this? (showed up in rebase)
//const COL1: Color = Color::Rgba(0.07, 0.1, 0.1, 0.9);

pub const TEXT_COLOR: iced::Color = iced::Color::from_rgb(1.0, 1.0, 1.0);
pub const DISABLED_TEXT_COLOR: iced::Color = iced::Color::from_rgba(1.0, 1.0, 1.0, 0.2);

pub const FILL_FRAC_ONE: f32 = 0.67;
pub const FILL_FRAC_TWO: f32 = 0.53;

image_ids_ice! {
    struct Imgs {
        <ImageGraphic>
        v_logo: "voxygen.element.v_logo",
        bg: "voxygen.background.bg_main",
        banner_top: "voxygen.element.ui.generic.frames.banner_top",
        banner_gradient_bottom: "voxygen.element.ui.generic.frames.banner_gradient_bottom",
        button: "voxygen.element.ui.generic.buttons.button",
        button_hover: "voxygen.element.ui.generic.buttons.button_hover",
        button_press: "voxygen.element.ui.generic.buttons.button_press",
        input_bg: "voxygen.element.ui.generic.textbox",
        loading_art: "voxygen.element.ui.generic.frames.loading_screen.loading_bg",
        loading_art_l: "voxygen.element.ui.generic.frames.loading_screen.loading_bg_l",
        loading_art_r: "voxygen.element.ui.generic.frames.loading_screen.loading_bg_r",
        selection: "voxygen.element.ui.generic.frames.selection",
        selection_hover: "voxygen.element.ui.generic.frames.selection_hover",
        selection_press: "voxygen.element.ui.generic.frames.selection_press",
    }
}

// Randomly loaded background images
const BG_IMGS: [&str; 14] = [
    "voxygen.background.bg_1",
    "voxygen.background.bg_2",
    "voxygen.background.bg_3",
    "voxygen.background.bg_4",
    "voxygen.background.bg_5",
    "voxygen.background.bg_6",
    "voxygen.background.bg_7",
    "voxygen.background.bg_8",
    "voxygen.background.bg_9",
    "voxygen.background.bg_10",
    "voxygen.background.bg_11",
    "voxygen.background.bg_12",
    "voxygen.background.bg_13",
    "voxygen.background.bg_14",
];

pub enum Event {
    LoginAttempt {
        username: String,
        password: String,
        server_address: String,
    },
    CancelLoginAttempt,
    ChangeLanguage(LanguageMetadata),
    Quit,
    DeleteServer {
        server_index: usize,
    },
}

pub struct LoginInfo {
    pub username: String,
    pub password: String,
    pub server: String,
}

enum ConnectionState {
    InProgress,
}

enum Screen {
    // Note: Keeping in case we re-add the disclaimer
    /*Disclaimer {
        screen: disclaimer::Screen,
    },*/
    Credits {
        screen: credits::Screen,
    },
    Login {
        screen: Box<login::Screen>, // boxed to avoid large variant
        // Error to display in a box
        error: Option<String>,
    },
    Servers {
        screen: servers::Screen,
    },
    Connecting {
        screen: connecting::Screen,
        connection_state: ConnectionState,
    },
}

struct Controls {
    fonts: Fonts,
    imgs: Imgs,
    bg_img: widget::image::Handle,
    i18n: LocalizationHandle,
    // Voxygen version
    version: String,
    // Alpha disclaimer
    alpha: String,
    credits: Credits,

    selected_server_index: Option<usize>,
    login_info: LoginInfo,

    is_selecting_language: bool,
    selected_language_index: Option<usize>,

    time: f64,

    screen: Screen,
}

#[derive(Clone)]
enum Message {
    Quit,
    Back,
    ShowServers,
    ShowCredits,
    Multiplayer,
    LanguageChanged(usize),
    OpenLanguageMenu,
    Username(String),
    Password(String),
    Server(String),
    ServerChanged(usize),
    FocusPassword,
    CancelConnect,
    CloseError,
    DeleteServer,
    /* Note: Keeping in case we re-add the disclaimer
     *AcceptDisclaimer, */
}

impl Controls {
    fn new(
        fonts: Fonts,
        imgs: Imgs,
        bg_img: widget::image::Handle,
        i18n: LocalizationHandle,
        settings: &Settings,
    ) -> Self {

        log::info!("MainUI Controls new");
        let version = common::util::DISPLAY_VERSION_LONG.clone();
        let alpha = format!("Veloren {}", common::util::DISPLAY_VERSION.as_str());
        let credits = Credits::load_expect_cloned("common.credits");

        log::info!("MainUI Controls new: Screen::Login");

        let screen = Screen::Login {
            screen: Box::new(login::Screen::new()),
            error: None,
        };

        log::info!("MainUI Controls new: LoginInfo");
        let login_info = LoginInfo {
            username: settings.networking.username.clone(),
            password: String::new(),
            server: settings.networking.default_server.clone(),
        };

        log::info!("MainUI Controls new: selected_server_index");
        let selected_server_index = settings
            .networking
            .servers
            .iter()
            .position(|f| f == &login_info.server);

        let language_metadatas = i18n::list_localizations();
        let selected_language_index = language_metadatas
            .iter()
            .position(|f| f.language_identifier == settings.language.selected_language);

        log::info!("MainUI Controls new: over");

        Self {
            fonts,
            imgs,
            bg_img,
            i18n,
            version,
            alpha,
            credits,

            selected_server_index,
            login_info,

            is_selecting_language: false,
            selected_language_index,

            time: 0.0,

            screen,
        }
    }

    fn view(
        &mut self,
        settings: &Settings,
        key_layout: &Option<KeyLayout>,
        dt: f32,
    ) -> Element<Message> {
        self.time += dt as f64;

        // TODO: consider setting this as the default in the renderer
        let button_style = style::button::Style::new(self.imgs.button)
            .hover_image(self.imgs.button_hover)
            .press_image(self.imgs.button_press)
            .text_color(TEXT_COLOR)
            .disabled_text_color(DISABLED_TEXT_COLOR);

        let alpha = Text::new(&self.alpha)
            .size(self.fonts.cyri.scale(12))
            .width(Length::Fill)
            .horizontal_alignment(Horizontal::Center);

        let top_text = Row::with_children(vec![
            Space::new(Length::Fill, Length::Shrink).into(),
            alpha.into(),
            if matches!(&self.screen, Screen::Login { .. }) {
                // Login screen shows the Velroen logo over the version
                Space::new(Length::Fill, Length::Shrink).into()
            } else {
                Text::new(&self.version)
                    .size(self.fonts.cyri.scale(15))
                    .width(Length::Fill)
                    .horizontal_alignment(Horizontal::Right)
                    .into()
            },
        ])
        .padding(3)
        .width(Length::Fill);

        let bg_img = if matches!(&self.screen, Screen::Connecting { .. }) {
            self.bg_img
        } else {
            self.imgs.bg
        };

        let language_metadatas = i18n::list_localizations();

        // TODO: make any large text blocks scrollable so that if the area is to
        // small they can still be read
        let content = match &mut self.screen {
            // Note: Keeping in case we re-add the disclaimer
            //Screen::Disclaimer { screen } => screen.view(&self.fonts, &self.i18n, button_style),
            Screen::Credits { screen } => {
                screen.view(&self.fonts, &self.i18n.read(), &self.credits, button_style)
            },
            Screen::Login { screen, error } => screen.view(
                &self.fonts,
                &self.imgs,
                &self.login_info,
                error.as_deref(),
                &self.i18n.read(),
                self.is_selecting_language,
                self.selected_language_index,
                &language_metadatas,
                button_style,
                &self.version,
            ),
            Screen::Servers { screen } => screen.view(
                &self.fonts,
                &self.imgs,
                &settings.networking.servers,
                self.selected_server_index,
                &self.i18n.read(),
                button_style,
            ),
            Screen::Connecting {
                screen,
                connection_state,
            } => screen.view(
                &self.fonts,
                &self.imgs,
                connection_state,
                self.time,
                &self.i18n.read(),
                button_style,
                settings.interface.loading_tips,
                &settings.controls,
                key_layout,
            ),
        };

        Container::new(
            Column::with_children(vec![top_text.into(), content])
                .spacing(3)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .style(style::container::Style::image(bg_img))
        .into()
    }

    fn update(
        &mut self,
        message: Message,
        events: &mut Vec<Event>,
        settings: &Settings,
        ui: &mut Ui,
    ) {
        let servers = &settings.networking.servers;
        let mut language_metadatas = i18n::list_localizations();

        match message {
            Message::Quit => events.push(Event::Quit),
            Message::Back => {
                self.screen = Screen::Login {
                    screen: Box::new(login::Screen::new()),
                    error: None,
                };
            },
            Message::ShowServers => {
                if matches!(&self.screen, Screen::Login { .. }) {
                    self.selected_server_index =
                        servers.iter().position(|f| f == &self.login_info.server);
                    self.screen = Screen::Servers {
                        screen: servers::Screen::new(),
                    };
                }
            },
            Message::ShowCredits => {
                self.screen = Screen::Credits {
                    screen: credits::Screen::new(),
                };
            },
            Message::Multiplayer => {
                self.screen = Screen::Connecting {
                    screen: connecting::Screen::new(ui),
                    connection_state: ConnectionState::InProgress,
                };

                events.push(Event::LoginAttempt {
                    username: self.login_info.username.trim().to_string(),
                    password: self.login_info.password.clone(),
                    server_address: self.login_info.server.clone(),
                });
            },
            Message::Username(new_value) => self.login_info.username = new_value,
            Message::LanguageChanged(new_value) => {
                events.push(Event::ChangeLanguage(language_metadatas.remove(new_value)));
            },
            Message::OpenLanguageMenu => self.is_selecting_language = !self.is_selecting_language,
            Message::Password(new_value) => self.login_info.password = new_value,
            Message::Server(new_value) => {
                self.login_info.server = new_value;
            },
            Message::ServerChanged(new_value) => {
                self.selected_server_index = Some(new_value);
                self.login_info.server = servers[new_value].clone();
            },
            Message::FocusPassword => {
                if let Screen::Login { screen, .. } = &mut self.screen {
                    screen.banner.password = text_input::State::focused();
                    screen.banner.username = text_input::State::new();
                }
            },
            Message::CancelConnect => {
                self.exit_connect_screen();
                events.push(Event::CancelLoginAttempt);
            },
            Message::CloseError => {
                if let Screen::Login { error, .. } = &mut self.screen {
                    *error = None;
                }
            },
            Message::DeleteServer => {
                if let Some(server_index) = self.selected_server_index {
                    events.push(Event::DeleteServer { server_index });
                }
            },
        }
    }

    // Connection successful of failed
    fn exit_connect_screen(&mut self) {
        if matches!(&self.screen, Screen::Connecting { .. }) {
            self.screen = Screen::Login {
                screen: Box::new(login::Screen::new()),
                error: None,
            }
        }
    }

    fn connection_error(&mut self, error: String) {
        if matches!(&self.screen, Screen::Connecting { .. })
            || matches!(&self.screen, Screen::Login { .. })
        {
            self.screen = Screen::Login {
                screen: Box::new(login::Screen::new()),
                error: Some(error),
            }
        } else {
            log::warn!("connection_error invoked on unhandled screen!");
        }
    }

    fn tab(&mut self) {
        if let Screen::Login { screen, .. } = &mut self.screen {
            // TODO: add select all function in iced
            if screen.banner.username.is_focused() {
                screen.banner.username = text_input::State::new();
                screen.banner.password = text_input::State::focused();
                screen.banner.password.move_cursor_to_end();
            } else if screen.banner.password.is_focused() {
                screen.banner.password = text_input::State::new();
                screen.banner.server = text_input::State::focused();
                screen.banner.server.move_cursor_to_end();
            } else if screen.banner.server.is_focused() {
                screen.banner.server = text_input::State::new();
                screen.banner.username = text_input::State::focused();
                screen.banner.username.move_cursor_to_end();
            }
        }
    }
}

pub struct MainMenuUi {
    ui: Ui,
    // TODO: re add this
    // tip_no: u16,
    controls: Controls,
}

impl MainMenuUi {
    pub fn new(global_state: &mut GlobalState) -> Self {

        log::info!("MainMenuUi new start");
        // Load language
        let i18n = &global_state.i18n.read();
        // TODO: don't add default font twice
        let font = load_font(&i18n.fonts().get("cyri").unwrap().asset_key);

        log::info!("MainMenuUi New UI start");
        let mut ui = Ui::new(
            &mut global_state.window,
            font,
            global_state.settings.interface.ui_scale,
        )
        .unwrap();

        log::info!("MainMenuUi LoadFont start");
        let fonts = Fonts::load(i18n.fonts(), &mut ui).expect("Impossible to load fonts");

        log::info!("MainMenuUi bg_img_spec start");
        let bg_img_spec = BG_IMGS.choose(&mut thread_rng()).unwrap();

        log::info!("MainMenuUi bg_img start");
        let bg_img = assets::Image::load_expect(bg_img_spec).read().to_image();
        let controls = Controls::new(
            fonts,
            Imgs::load(&mut ui).expect("Failed to load images"),
            ui.add_graphic(Graphic::Image(bg_img, None)),
            global_state.i18n,
            &global_state.settings,
        );

        log::info!("MainMenuUi New End");

        Self { ui, controls }
    }

    pub fn update_language(&mut self, i18n: LocalizationHandle, settings: &Settings) {
        self.controls.i18n = i18n;
        let i18n = &i18n.read();
        let font = load_font(&i18n.fonts().get("cyri").unwrap().asset_key);
        self.ui.clear_fonts(font);
        self.controls.fonts =
            Fonts::load(i18n.fonts(), &mut self.ui).expect("Impossible to load fonts!");
        let language_metadatas = i18n::list_localizations();
        self.controls.selected_language_index = language_metadatas
            .iter()
            .position(|f| f.language_identifier == settings.language.selected_language);
    }

    pub fn show_info(&mut self, msg: String) { self.controls.connection_error(msg); }

    pub fn connected(&mut self) { self.controls.exit_connect_screen(); }

    pub fn cancel_connection(&mut self) { self.controls.exit_connect_screen(); }

    pub fn handle_event(&mut self, event: window::Event) -> bool {
        match event {
            // Pass events to ui.
            window::Event::IcedUi(event) => {
                self.handle_ui_event(event);
                true
            },
            window::Event::ScaleFactorChanged(s) => {
                self.ui.scale_factor_changed(s);
                false
            },
            _ => false,
        }
    }

    pub fn handle_ui_event(&mut self, event: ui::ice::Event) {
        // Tab for input fields
        use iced::keyboard;
        if matches!(
            &event,
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key_code: keyboard::KeyCode::Tab,
                ..
            })
        ) {
            self.controls.tab();
        }

        self.ui.handle_event(event);
    }

    pub fn set_scale_mode(&mut self, scale_mode: ui::ScaleMode) {
        self.ui.set_scaling_mode(scale_mode);
    }

    pub fn maintain(&mut self, global_state: &mut GlobalState, dt: Duration) -> Vec<Event> {
        let mut events = Vec::new();

        let (messages, _) = self.ui.maintain(
            self.controls.view(
                &global_state.settings,
                &global_state.window.key_layout,
                dt.as_secs_f32(),
            ),
            global_state.window.renderer_mut(),
            None,
            &mut global_state.clipboard,
        );

        messages.into_iter().for_each(|message| {
            self.controls
                .update(message, &mut events, &global_state.settings, &mut self.ui)
        });

        events
    }

    pub fn render<'a>(&'a self, drawer: &mut ThirdPassDrawer<'a>) { self.ui.render(drawer); }
}
