// Copyright 2024 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

// False-positive with dev-dependencies only used in examples
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(unnameable_types, unreachable_pub)]
#![warn(clippy::print_stdout, clippy::print_stderr)]
use std::{collections::HashMap, sync::Arc};

use masonry::{
    dpi::LogicalSize,
    event_loop_runner,
    widget::{RootWidget, WidgetMut},
    Widget, WidgetId, WidgetPod,
};
use winit::{
    error::EventLoopError,
    window::{Window, WindowAttributes},
};
use xilem_core::{
    AsyncCtx, MessageResult, RawProxy, SuperElement, View, ViewElement, ViewId, ViewPathTracker,
};

pub use masonry::{
    dpi,
    event_loop_runner::{EventLoop, EventLoopBuilder},
    Color, TextAlignment,
};
pub use xilem_core as core;

mod one_of;

mod any_view;
pub use any_view::AnyWidgetView;

mod driver;
pub use driver::{async_action, MasonryDriver, MasonryProxy, ASYNC_MARKER_WIDGET};

pub mod view;

/// Tokio is the async runner used with Xilem.
pub use tokio;

pub struct Xilem<State, Logic> {
    state: State,
    logic: Logic,
    runtime: tokio::runtime::Runtime,
    background_color: Color,
}

impl<State, Logic, View> Xilem<State, Logic>
where
    Logic: FnMut(&mut State) -> View,
    View: WidgetView<State>,
{
    pub fn new(state: State, logic: Logic) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        Xilem {
            state,
            logic,
            runtime,
            background_color: Color::BLACK,
        }
    }

    /// Sets main window background color.
    pub fn background_color(mut self, color: Color) -> Self {
        self.background_color = color;
        self
    }

    // TODO: Make windows a specific view
    pub fn run_windowed(
        self,
        // We pass in the event loop builder to allow
        // This might need to be generic over the event type?
        event_loop: EventLoopBuilder,
        window_title: String,
    ) -> Result<(), EventLoopError>
    where
        State: 'static,
        Logic: 'static,
        View: 'static,
    {
        let window_size = LogicalSize::new(600., 800.);
        let window_attributes = Window::default_attributes()
            .with_title(window_title)
            .with_resizable(true)
            .with_min_inner_size(window_size);
        self.run_windowed_in(event_loop, window_attributes)
    }

    // TODO: Make windows into a custom view
    pub fn run_windowed_in(
        self,
        mut event_loop: EventLoopBuilder,
        window_attributes: WindowAttributes,
    ) -> Result<(), EventLoopError>
    where
        State: 'static,
        Logic: 'static,
        View: 'static,
    {
        let event_loop = event_loop.build()?;
        let proxy = event_loop.create_proxy();
        let bg_color = self.background_color;
        let (root_widget, driver) = self.into_driver(Arc::new(MasonryProxy(proxy)));
        event_loop_runner::run_with(event_loop, window_attributes, root_widget, driver, bg_color)
    }

    pub fn into_driver(
        mut self,
        proxy: Arc<dyn RawProxy>,
    ) -> (
        impl Widget,
        MasonryDriver<State, Logic, View, View::ViewState>,
    ) {
        let first_view = (self.logic)(&mut self.state);
        let mut ctx = ViewCtx {
            widget_map: WidgetMap::default(),
            id_path: Vec::new(),
            view_tree_changed: false,
            proxy,
            runtime: self.runtime,
        };
        let (pod, view_state) = first_view.build(&mut ctx);
        let root_widget = RootWidget::from_pod(pod.inner);
        let driver = MasonryDriver {
            current_view: first_view,
            logic: self.logic,
            state: self.state,
            ctx,
            view_state,
        };
        (root_widget, driver)
    }
}

/// A container for a [Masonry](masonry) widget to be used with Xilem.
///
/// Equivalent to [`WidgetPod<W>`], but in the [`xilem`](crate) crate to work around the orphan rule.
pub struct Pod<W: Widget> {
    pub inner: WidgetPod<W>,
}

impl<W: Widget> Pod<W> {
    /// Create a new `Pod` for `inner`.
    pub fn new(inner: W) -> Self {
        Self::from(WidgetPod::new(inner))
    }
}

impl<W: Widget> From<WidgetPod<W>> for Pod<W> {
    fn from(inner: WidgetPod<W>) -> Self {
        Pod { inner }
    }
}

impl<W: Widget> ViewElement for Pod<W> {
    type Mut<'a> = WidgetMut<'a, W>;
}

impl<W: Widget> SuperElement<Pod<W>> for Pod<Box<dyn Widget>> {
    fn upcast(child: Pod<W>) -> Self {
        child.inner.boxed().into()
    }

    fn with_downcast_val<R>(
        mut this: Self::Mut<'_>,
        f: impl FnOnce(<Pod<W> as xilem_core::ViewElement>::Mut<'_>) -> R,
    ) -> (Self::Mut<'_>, R) {
        let downcast = this.downcast();
        let ret = f(downcast);
        (this, ret)
    }
}

pub trait WidgetView<State, Action = ()>:
    View<State, Action, ViewCtx, Element = Pod<Self::Widget>> + Send + Sync
{
    type Widget: Widget;

    /// Returns a boxed type erased [`AnyWidgetView`]
    ///
    /// # Examples
    /// ```
    /// use xilem::{view::label, WidgetView};
    ///
    /// # fn view<State: 'static>() -> impl WidgetView<State> {
    /// label("a label").boxed()
    /// # }
    ///
    /// ```
    fn boxed(self) -> Box<AnyWidgetView<State, Action>>
    where
        State: 'static,
        Action: 'static,
        Self: Sized,
    {
        Box::new(self)
    }
}

impl<V, State, Action, W> WidgetView<State, Action> for V
where
    V: View<State, Action, ViewCtx, Element = Pod<W>> + Send + Sync,
    W: Widget,
{
    type Widget = W;
}

type WidgetMap = HashMap<WidgetId, Vec<ViewId>>;

pub struct ViewCtx {
    /// The map from a widgets id to its position in the View tree.
    ///
    /// This includes only the widgets which might send actions
    widget_map: WidgetMap,
    id_path: Vec<ViewId>,
    view_tree_changed: bool,
    proxy: Arc<dyn RawProxy>,
    runtime: tokio::runtime::Runtime,
}

impl ViewPathTracker for ViewCtx {
    fn push_id(&mut self, id: xilem_core::ViewId) {
        self.id_path.push(id);
    }

    fn pop_id(&mut self) {
        self.id_path.pop();
    }

    fn view_path(&mut self) -> &[xilem_core::ViewId] {
        &self.id_path
    }
}

impl ViewCtx {
    pub fn mark_changed(&mut self) {
        if cfg!(debug_assertions) {
            self.view_tree_changed = true;
        }
    }

    pub fn with_leaf_action_widget<E: Widget>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Pod<E>,
    ) -> (Pod<E>, ()) {
        (self.with_action_widget(f), ())
    }

    pub fn with_action_widget<E: Widget>(&mut self, f: impl FnOnce(&mut Self) -> Pod<E>) -> Pod<E> {
        let value = f(self);
        let id = value.inner.id();
        let path = self.id_path.clone();
        self.widget_map.insert(id, path);
        value
    }

    pub fn teardown_leaf<E: Widget>(&mut self, widget: WidgetMut<E>) {
        self.widget_map.remove(&widget.ctx.widget_id());
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }
}

impl AsyncCtx for ViewCtx {
    fn proxy(&mut self) -> Arc<dyn RawProxy> {
        self.proxy.clone()
    }
}
