//! Thin widget wrapper that enables IME for the terminal canvas by calling
//! `shell.request_input_method` each event cycle.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::advanced::InputMethod;
use iced::advanced::input_method;
use iced::{Element, Event, Length, Rectangle, Size, Vector};
use iced::mouse;

pub struct ImeEnabled<'a, M, T, R>(Element<'a, M, T, R>);

impl<'a, M, T, R> ImeEnabled<'a, M, T, R> {
    pub fn new(el: impl Into<Element<'a, M, T, R>>) -> Self {
        Self(el.into())
    }
}

impl<'a, M, T, R: renderer::Renderer> Widget<M, T, R> for ImeEnabled<'a, M, T, R> {
    fn tag(&self) -> tree::Tag { tree::Tag::stateless() }
    fn state(&self) -> tree::State { tree::State::None }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.0)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.0));
    }

    fn size(&self) -> Size<Length> { self.0.as_widget().size() }
    fn size_hint(&self) -> Size<Length> { self.0.as_widget().size_hint() }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
        self.0.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(&self, tree: &Tree, renderer: &mut R, theme: &T, style: &renderer::Style,
            layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle) {
        self.0.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport)
    }

    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout<'_>,
              cursor: mouse::Cursor, renderer: &R, clipboard: &mut dyn Clipboard,
              shell: &mut Shell<'_, M>, viewport: &Rectangle) {
        shell.request_input_method::<String>(&InputMethod::Enabled {
            cursor: layout.bounds(),
            purpose: input_method::Purpose::Terminal,
            preedit: None,
        });
        self.0.as_widget_mut().update(
            &mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor,
                         viewport: &Rectangle, renderer: &R) -> mouse::Interaction {
        self.0.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(&'b mut self, tree: &'b mut Tree, layout: Layout<'b>, renderer: &R,
                   viewport: &Rectangle, translation: Vector)
        -> Option<overlay::Element<'b, M, T, R>> {
        self.0.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, M: 'a, T: 'a, R: renderer::Renderer + 'a> From<ImeEnabled<'a, M, T, R>>
    for Element<'a, M, T, R>
{
    fn from(w: ImeEnabled<'a, M, T, R>) -> Self { Element::new(w) }
}
