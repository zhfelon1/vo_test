use super::super::{IcedRenderer, Primitive};
use iced::{mouse, widget::space, Rectangle};

impl space::Renderer for IcedRenderer {
    fn draw(&mut self, _bounds: Rectangle) -> Self::Output {
        (Primitive::Nothing, mouse::Interaction::default())
    }
}
