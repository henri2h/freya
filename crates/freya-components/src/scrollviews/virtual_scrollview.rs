use std::{ops::Range, time::Duration};

use freya_core::prelude::*;
use freya_sdk::timeout::use_timeout;
use torin::{geometry::CursorPoint, node::Node, prelude::Direction, size::Size};

use crate::scrollviews::{
    ScrollBar, ScrollConfig, ScrollController, ScrollThumb,
    scroll_physics::{MIN_FLING_VELOCITY, VelocityTracker, momentum_scroll},
    shared::{
        Axis, get_container_sizes, get_corrected_scroll_position, get_scroll_position_from_cursor,
        get_scroll_position_from_wheel, get_scrollbar_pos_and_size, handle_key_event,
        is_scrollbar_visible,
    },
    use_scroll_controller,
};

/// Controls how [`VirtualScrollView`] determines the size (height for vertical scrolling, width
/// for horizontal) of each item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ItemSize {
    /// Every item has exactly this size.
    Fixed(f32),
    /// Items may have different sizes. Not-yet-measured items render at `estimate`; once an
    /// item is laid out its real size is cached and the scroll position is corrected so
    /// already-scrolled-past content doesn't jump.
    Dynamic {
        /// Initial size used for items that haven't been measured yet.
        estimate: f32,
    },
}

impl From<f32> for ItemSize {
    fn from(value: f32) -> Self {
        ItemSize::Fixed(value)
    }
}

/// One-direction scrollable area that dynamically builds and renders items based in their size and current available size,
/// this is intended for apps using large sets of data that need good performance.
///
/// Unlike [`ScrollView`](crate::scrollviews::ScrollView), which lays out every child even when it is
/// off screen, a `VirtualScrollView` takes a builder closure and only calls it for the items that
/// are actually visible, so the cost stays roughly constant no matter how long the list is.
///
/// It needs two things to know which items fall inside the viewport:
/// [`item_size`](VirtualScrollView::item_size), the fixed size of each item along the scroll axis,
/// and [`length`](VirtualScrollView::length), the total number of items.
///
/// # Example
///
/// ```rust
/// # use freya::prelude::*;
/// fn app() -> impl IntoElement {
///     rect().child(
///         VirtualScrollView::new(|i, _| {
///             rect()
///                 .key(i)
///                 .height(Size::px(25.))
///                 .padding(4.)
///                 .child(format!("Item {i}"))
///                 .into()
///         })
///         .length(300usize)
///         .item_size(25.),
///     )
/// }
///
/// # use freya_testing::prelude::*;
/// # launch_doc(|| {
/// #   rect().center().expanded().child(app())
/// # }, "./images/gallery_virtual_scrollview.png").with_hook(|t| {
/// #   t.move_cursor((125., 115.));
/// #   t.sync_and_update();
/// # });
/// ```
///
/// # Preview
/// ![VirtualScrollView Preview][virtual_scrollview]
///
/// # Dynamic item sizes
///
/// When items don't all share the same size (e.g. a chat timeline with wrapping text), use
/// [`ItemSize::Dynamic`] instead of a fixed size. Unmeasured items render at `estimate`; once an
/// item is laid out its real size is cached and the scroll position is corrected so
/// already-scrolled-past content doesn't jump.
///
/// ```rust
/// # use freya::prelude::*;
/// fn app() -> impl IntoElement {
///     VirtualScrollView::new(|i, _| {
///         rect()
///             .key(i)
///             .padding(4.)
///             .child(format!("Item {i}, this text might wrap onto multiple lines"))
///             .into()
///     })
///     .length(300usize)
///     .item_size(ItemSize::Dynamic { estimate: 25. })
/// }
/// ```
#[cfg_attr(feature = "docs",
    doc = embed_doc_image::embed_image!("virtual_scrollview", "images/gallery_virtual_scrollview.png")
)]
/// Context provided to all descendants of a [`VirtualScrollView`] indicating whether a drag or
/// momentum scroll is currently active. Consume with `try_consume_context::<IsScrollingCtx>()`
/// and read `.0` reactively (`.0.read()`) so the consumer re-renders when the flag changes.
#[derive(Clone, Copy)]
pub struct IsScrollingCtx(pub State<bool>);

#[derive(Clone)]
pub struct VirtualScrollView<D, B: Fn(usize, &D) -> Element> {
    builder: B,
    builder_data: D,
    item_size: ItemSize,
    length: usize,
    layout: LayoutData,
    show_scrollbar: bool,
    scroll_with_arrows: bool,
    scroll_controller: Option<ScrollController>,
    invert_scroll_wheel: bool,
    drag_scrolling: bool,
    key: DiffKey,
}

impl<D: PartialEq, B: Fn(usize, &D) -> Element> LayoutExt for VirtualScrollView<D, B> {
    fn get_layout(&mut self) -> &mut LayoutData {
        &mut self.layout
    }
}

impl<D: PartialEq, B: Fn(usize, &D) -> Element> ContainerSizeExt for VirtualScrollView<D, B> {}

impl<D: PartialEq, B: Fn(usize, &D) -> Element> KeyExt for VirtualScrollView<D, B> {
    fn write_key(&mut self) -> &mut DiffKey {
        &mut self.key
    }
}

impl<D: PartialEq, B: Fn(usize, &D) -> Element> PartialEq for VirtualScrollView<D, B> {
    fn eq(&self, other: &Self) -> bool {
        self.builder_data == other.builder_data
            && self.item_size == other.item_size
            && self.length == other.length
            && self.layout == other.layout
            && self.show_scrollbar == other.show_scrollbar
            && self.scroll_with_arrows == other.scroll_with_arrows
            && self.scroll_controller == other.scroll_controller
            && self.invert_scroll_wheel == other.invert_scroll_wheel
    }
}

impl<B: Fn(usize, &()) -> Element> VirtualScrollView<(), B> {
    /// Creates a virtual scroll view that builds each item on demand from its index.
    pub fn new(builder: B) -> Self {
        Self {
            builder,
            builder_data: (),
            item_size: ItemSize::Fixed(0.),
            length: 0,
            layout: {
                let mut l = LayoutData::default();
                l.layout.width = Size::fill();
                l.layout.height = Size::fill();
                l
            },
            show_scrollbar: true,
            scroll_with_arrows: true,
            scroll_controller: None,
            invert_scroll_wheel: false,
            drag_scrolling: true,
            key: DiffKey::None,
        }
    }

    /// Like [`new`](Self::new) but driven by the given [`ScrollController`].
    pub fn new_controlled(builder: B, scroll_controller: ScrollController) -> Self {
        Self {
            builder,
            builder_data: (),
            item_size: ItemSize::Fixed(0.),
            length: 0,
            layout: {
                let mut l = LayoutData::default();
                l.layout.width = Size::fill();
                l.layout.height = Size::fill();
                l
            },
            show_scrollbar: true,
            scroll_with_arrows: true,
            scroll_controller: Some(scroll_controller),
            invert_scroll_wheel: false,
            drag_scrolling: true,
            key: DiffKey::None,
        }
    }
}

impl<D, B: Fn(usize, &D) -> Element> VirtualScrollView<D, B> {
    /// Like [`new`](Self::new) but passes shared `builder_data` to every item build.
    ///
    /// The builder closure cannot be compared across renders, so data captured inside it never
    /// triggers a rebuild. Passing the data here instead makes it part of the view's `PartialEq`,
    /// so the visible items are rebuilt whenever it changes.
    ///
    /// ```rust
    /// # use freya::prelude::*;
    /// fn app() -> impl IntoElement {
    ///     let items = use_state(|| vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    ///
    ///     // The current items are passed as data, so editing `items` rebuilds the visible rows.
    ///     VirtualScrollView::new_with_data(items.read().clone(), |i, items: &Vec<String>| {
    ///         rect()
    ///             .key(i)
    ///             .height(Size::px(25.))
    ///             .child(items[i].clone())
    ///             .into()
    ///     })
    ///     .length(items.read().len())
    ///     .item_size(25.)
    /// }
    /// ```
    pub fn new_with_data(builder_data: D, builder: B) -> Self {
        Self {
            builder,
            builder_data,
            item_size: ItemSize::Fixed(0.),
            length: 0,
            layout: Node {
                width: Size::fill(),
                height: Size::fill(),
                ..Default::default()
            }
            .into(),
            show_scrollbar: true,
            scroll_with_arrows: true,
            scroll_controller: None,
            invert_scroll_wheel: false,
            drag_scrolling: true,
            key: DiffKey::None,
        }
    }

    /// Like [`new_with_data`](Self::new_with_data) but driven by the given [`ScrollController`].
    pub fn new_with_data_controlled(
        builder_data: D,
        builder: B,
        scroll_controller: ScrollController,
    ) -> Self {
        Self {
            builder,
            builder_data,
            item_size: ItemSize::Fixed(0.),
            length: 0,

            layout: Node {
                width: Size::fill(),
                height: Size::fill(),
                ..Default::default()
            }
            .into(),
            show_scrollbar: true,
            scroll_with_arrows: true,
            scroll_controller: Some(scroll_controller),
            invert_scroll_wheel: false,
            drag_scrolling: true,
            key: DiffKey::None,
        }
    }

    /// Toggles whether the scrollbar is shown when the content overflows.
    pub fn show_scrollbar(mut self, show_scrollbar: bool) -> Self {
        self.show_scrollbar = show_scrollbar;
        self
    }

    /// Sets the axis the items flow and scroll in.
    pub fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }

    /// Toggles whether the arrow keys scroll the view while it is focused.
    pub fn scroll_with_arrows(mut self, scroll_with_arrows: impl Into<bool>) -> Self {
        self.scroll_with_arrows = scroll_with_arrows.into();
        self
    }

    /// Sets the fixed size of every item along the scroll axis, used to decide which items to render.
    pub fn item_size(mut self, item_size: impl Into<f32>) -> Self {
        self.item_size = item_size.into();
        self
    }

    /// Sets the total number of items the view can scroll through.
    pub fn length(mut self, length: impl Into<usize>) -> Self {
        self.length = length.into();
        self
    }

    /// Inverts the direction of the mouse wheel relative to the content.
    pub fn invert_scroll_wheel(mut self, invert_scroll_wheel: impl Into<bool>) -> Self {
        self.invert_scroll_wheel = invert_scroll_wheel.into();
        self
    }

    /// Toggles scrolling by dragging the content, useful mainly for touch input.
    pub fn drag_scrolling(mut self, drag_scrolling: bool) -> Self {
        self.drag_scrolling = drag_scrolling;
        self
    }

    /// Attaches a [`ScrollController`] to drive this view externally.
    pub fn scroll_controller(
        mut self,
        scroll_controller: impl Into<Option<ScrollController>>,
    ) -> Self {
        self.scroll_controller = scroll_controller.into();
        self
    }

    /// Caps the width of the scroll view.
    pub fn max_width(mut self, max_width: impl Into<Size>) -> Self {
        self.layout.maximum_width = max_width.into();
        self
    }

    /// Caps the height of the scroll view.
    pub fn max_height(mut self, max_height: impl Into<Size>) -> Self {
        self.layout.maximum_height = max_height.into();
        self
    }
}

impl<D: PartialEq + 'static, B: Fn(usize, &D) -> Element + 'static> Component
    for VirtualScrollView<D, B>
{
    fn render(self: &VirtualScrollView<D, B>) -> impl IntoElement {
        let a11y_id = use_a11y();
        let mut timeout = use_timeout(|| Duration::from_millis(800));
        let mut pressing_shift = use_state(|| false);
        let mut clicking_scrollbar = use_state::<Option<(Axis, f64)>>(|| None);
        let mut size = use_state(SizedEventData::default);
        let mut scroll_controller = self
            .scroll_controller
            .unwrap_or_else(|| use_scroll_controller(ScrollConfig::default));
        let mut dragging_content = use_state::<Option<CursorPoint>>(|| None);
        let mut drag_origin = use_state::<Option<CursorPoint>>(|| None);
        let mut velocity_tracker = use_state(VelocityTracker::default);
        let mut momentum_task = use_state::<Option<TaskHandle>>(|| None);
        let mut suppress_next_press = use_state(|| false);
        let mut item_sizes = use_state(Vec::<f32>::new);
        let mut item_sizes_total = use_state(|| 0f32);
        let mut is_scrolling_state = use_state(|| false);
        use_provide_context(|| IsScrollingCtx(is_scrolling_state));
        let (scrolled_x, scrolled_y) = scroll_controller.into();
        let layout = &self.layout.layout;
        let direction = layout.direction;
        let drag_scrolling = self.drag_scrolling;

        // Keep the size cache in sync with `self.length` for `Dynamic` items, growing/shrinking
        // it (and the running total) as needed. New entries start at `estimate` until measured.
        if let ItemSize::Dynamic { estimate } = self.item_size {
            let current_len = item_sizes.peek().len();
            let target_len = self.length;
            if current_len != target_len {
                item_sizes.with_mut(|mut sizes| {
                    if target_len > current_len {
                        *item_sizes_total.write() += estimate * (target_len - current_len) as f32;
                        sizes.resize(target_len, estimate);
                    } else {
                        *item_sizes_total.write() -= sizes[target_len..].iter().sum::<f32>();
                        sizes.truncate(target_len);
                    }
                });
            }
        }

        let scrolled_axis_inner_size = match self.item_size {
            ItemSize::Fixed(item_size) => item_size * self.length as f32,
            ItemSize::Dynamic { .. } => *item_sizes_total.read(),
        };
        let (inner_width, inner_height) = match direction {
            Direction::Vertical => (size.read().inner_sizes.width, scrolled_axis_inner_size),
            Direction::Horizontal => (scrolled_axis_inner_size, size.read().inner_sizes.height),
        };

        scroll_controller.use_apply(inner_width, inner_height);

        let corrected_scrolled_x =
            get_corrected_scroll_position(inner_width, size.read().area.width(), scrolled_x as f32);

        let corrected_scrolled_y = get_corrected_scroll_position(
            inner_height,
            size.read().area.height(),
            scrolled_y as f32,
        );
        let horizontal_scrollbar_is_visible = !timeout.elapsed()
            && is_scrollbar_visible(self.show_scrollbar, inner_width, size.read().area.width());
        let vertical_scrollbar_is_visible = !timeout.elapsed()
            && is_scrollbar_visible(self.show_scrollbar, inner_height, size.read().area.height());

        let (scrollbar_x, scrollbar_width) =
            get_scrollbar_pos_and_size(inner_width, size.read().area.width(), corrected_scrolled_x);
        let (scrollbar_y, scrollbar_height) = get_scrollbar_pos_and_size(
            inner_height,
            size.read().area.height(),
            corrected_scrolled_y,
        );

        let (container_width, content_width) = get_container_sizes(self.layout.width.clone());
        let (container_height, content_height) = get_container_sizes(self.layout.height.clone());

        let scroll_with_arrows = self.scroll_with_arrows;
        let invert_scroll_wheel = self.invert_scroll_wheel;

        let on_capture_global_pointer_press = move |e: Event<PointerEventData>| {
            if clicking_scrollbar.read().is_some() {
                e.prevent_default();
                clicking_scrollbar.set(None);
            }

            if drag_scrolling {
                let was_dragging = dragging_content().is_some();
                let was_suppressed = *suppress_next_press.peek();
                suppress_next_press.set(false);

                if dragging_content().is_some() || drag_origin().is_some() {
                    dragging_content.set(None);
                    drag_origin.set(None);
                }

                if was_dragging || was_suppressed {
                    e.prevent_default();
                }

                if was_dragging {
                    let (vx, vy) = velocity_tracker.read().velocity();
                    velocity_tracker.write().clear();
                    let task_opt = *momentum_task.peek(); // extract before if-let to drop ReadRef
                    if let Some(task) = task_opt {
                        task.cancel();
                        momentum_task.set(None);
                    }
                    if vx.abs() > MIN_FLING_VELOCITY || vy.abs() > MIN_FLING_VELOCITY {
                        let viewport_w = size.read().area.width();
                        let viewport_h = size.read().area.height();
                        let task = spawn(async move {
                            momentum_scroll(
                                scroll_controller,
                                vx,
                                vy,
                                inner_width,
                                inner_height,
                                viewport_w,
                                viewport_h,
                            )
                            .await;
                            momentum_task.set(None);
                            is_scrolling_state.set(false);
                        });
                        momentum_task.set(Some(task));
                    } else {
                        is_scrolling_state.set(false);
                    }
                } else {
                    velocity_tracker.write().clear();
                }
            }
        };

        let on_wheel = move |e: Event<WheelEventData>| {
            // Only invert direction on deviced-sourced wheel events
            let invert_direction = e.source == WheelSource::Device
                && (*pressing_shift.read() || invert_scroll_wheel)
                && (!*pressing_shift.read() || !invert_scroll_wheel);

            let (x_movement, y_movement) = if invert_direction {
                (e.delta_y as f32, e.delta_x as f32)
            } else {
                (e.delta_x as f32, e.delta_y as f32)
            };

            // Vertical scroll
            let scroll_position_y = get_scroll_position_from_wheel(
                y_movement,
                inner_height,
                size.read().area.height(),
                corrected_scrolled_y,
            );
            scroll_controller.scroll_to_y(scroll_position_y).then(|| {
                e.stop_propagation();
            });

            // Horizontal scroll
            let scroll_position_x = get_scroll_position_from_wheel(
                x_movement,
                inner_width,
                size.read().area.width(),
                corrected_scrolled_x,
            );
            scroll_controller.scroll_to_x(scroll_position_x).then(|| {
                e.stop_propagation();
            });
            timeout.reset();
            let task_opt = *momentum_task.peek();
            if let Some(task) = task_opt {
                task.cancel();
                momentum_task.set(None);
                is_scrolling_state.set(false);
            }
        };

        let on_mouse_move = move |_| {
            timeout.reset();
        };

        let on_capture_global_pointer_move = move |e: Event<PointerEventData>| {
            if drag_scrolling {
                if let Some(prev) = dragging_content() {
                    let coords = e.global_location();
                    let delta = prev - coords;

                    scroll_controller.scroll_to_y((corrected_scrolled_y - delta.y as f32) as i32);
                    scroll_controller.scroll_to_x((corrected_scrolled_x - delta.x as f32) as i32);

                    dragging_content.set(Some(coords));
                    velocity_tracker.write().push(coords);
                    e.prevent_default();
                    timeout.reset();
                    a11y_id.request_focus();
                    return;
                } else if let Some(origin) = drag_origin() {
                    let coords = e.global_location();
                    let distance = (origin - coords).abs();

                    // Small threshold so taps can reach children (e.g. hover on buttons)
                    // without being immediately consumed by drag scrolling.
                    const DRAG_THRESHOLD: f64 = 2.0;

                    if distance.x > DRAG_THRESHOLD || distance.y > DRAG_THRESHOLD {
                        let delta = origin - coords;

                        scroll_controller
                            .scroll_to_y((corrected_scrolled_y - delta.y as f32) as i32);
                        scroll_controller
                            .scroll_to_x((corrected_scrolled_x - delta.x as f32) as i32);

                        dragging_content.set(Some(coords));
                        is_scrolling_state.set(true);
                        velocity_tracker.write().push(coords);
                        e.prevent_default();
                        timeout.reset();
                        a11y_id.request_focus();
                    }
                    return;
                }
            }

            let clicking_scrollbar = clicking_scrollbar.peek();

            if let Some((Axis::Y, y)) = *clicking_scrollbar {
                let coordinates = e.element_location();
                let cursor_y = coordinates.y - y - size.read().area.min_y() as f64;

                let scroll_position = get_scroll_position_from_cursor(
                    cursor_y as f32,
                    inner_height,
                    size.read().area.height(),
                );

                scroll_controller.scroll_to_y(scroll_position);
            } else if let Some((Axis::X, x)) = *clicking_scrollbar {
                let coordinates = e.element_location();
                let cursor_x = coordinates.x - x - size.read().area.min_x() as f64;

                let scroll_position = get_scroll_position_from_cursor(
                    cursor_x as f32,
                    inner_width,
                    size.read().area.width(),
                );

                scroll_controller.scroll_to_x(scroll_position);
            }

            if clicking_scrollbar.is_some() {
                e.prevent_default();
                timeout.reset();
                a11y_id.request_focus();
            }
        };

        let on_key_down = move |e: Event<KeyboardEventData>| {
            if !scroll_with_arrows
                && (e.key == Key::Named(NamedKey::ArrowUp)
                    || e.key == Key::Named(NamedKey::ArrowRight)
                    || e.key == Key::Named(NamedKey::ArrowDown)
                    || e.key == Key::Named(NamedKey::ArrowLeft))
            {
                return;
            }
            let x = corrected_scrolled_x;
            let y = corrected_scrolled_y;
            let inner_height = inner_height;
            let inner_width = inner_width;
            let viewport_height = size.read().area.height();
            let viewport_width = size.read().area.width();
            if let Some((x, y)) = handle_key_event(
                &e.key,
                (x, y),
                inner_height,
                inner_width,
                viewport_height,
                viewport_width,
                direction,
            ) {
                scroll_controller.scroll_to_x(x as i32);
                scroll_controller.scroll_to_y(y as i32);
                e.stop_propagation();
                timeout.reset();
            }
        };

        let on_global_key_down = move |e: Event<KeyboardEventData>| {
            let data = e;
            if data.key == Key::Named(NamedKey::Shift) {
                pressing_shift.set(true);
            }
        };

        let on_global_key_up = move |e: Event<KeyboardEventData>| {
            let data = e;
            if data.key == Key::Named(NamedKey::Shift) {
                pressing_shift.set(false);
            }
        };

        let (viewport_size, scroll_position) = if direction == Direction::vertical() {
            (size.read().area.height(), corrected_scrolled_y)
        } else {
            (size.read().area.width(), corrected_scrolled_x)
        };

        let (render_range, start_offset) = match self.item_size {
            ItemSize::Fixed(item_size) => {
                let range = get_render_range(
                    viewport_size,
                    scroll_position,
                    item_size,
                    self.length as f32,
                );
                let start_offset = (-scroll_position / item_size).floor() * item_size;
                (range, start_offset)
            }
            ItemSize::Dynamic { .. } => {
                let visible =
                    get_dynamic_render_range(viewport_size, scroll_position, &item_sizes.read());
                (visible.range, visible.start_offset)
            }
        };

        let first_visible_index = render_range.start;

        let children = render_range
            .map(|i| {
                let item = (self.builder)(i, &self.builder_data);

                match self.item_size {
                    ItemSize::Fixed(_) => item,
                    ItemSize::Dynamic { .. } => {
                        let on_item_sized = move |e: Event<SizedEventData>| {
                            let measured = match direction {
                                Direction::Vertical => e.area.height(),
                                Direction::Horizontal => e.area.width(),
                            };
                            if measured <= 0.0 {
                                return;
                            }

                            let old_size = match item_sizes.peek().get(i) {
                                Some(size) => *size,
                                // The list shrank since this item was scheduled to be measured.
                                None => return,
                            };
                            let delta = measured - old_size;
                            if delta.abs() < 0.5 {
                                return;
                            }

                            item_sizes.with_mut(|mut sizes| sizes[i] = measured);
                            *item_sizes_total.write() += delta;

                            // Correct the scroll position so already-scrolled-past content
                            // doesn't visually jump when its real size differs from the cache.
                            let (live_x, live_y): (i32, i32) = scroll_controller.into();
                            let scrolled_past_first = match direction {
                                Direction::Vertical => live_y != 0,
                                Direction::Horizontal => live_x != 0,
                            };
                            let needs_correction = i < first_visible_index
                                || (i == first_visible_index && scrolled_past_first);

                            if needs_correction {
                                match direction {
                                    Direction::Vertical => {
                                        scroll_controller.scroll_to_y(live_y - delta as i32)
                                    }
                                    Direction::Horizontal => {
                                        scroll_controller.scroll_to_x(live_x - delta as i32)
                                    }
                                };
                                if let Some(task) = *momentum_task.peek() {
                                    task.cancel();
                                    momentum_task.set(None);
                                }
                            }
                        };

                        rect()
                            .key(i)
                            .width(if direction == Direction::Vertical {
                                Size::fill()
                            } else {
                                Size::Inner
                            })
                            .height(if direction == Direction::Vertical {
                                Size::Inner
                            } else {
                                Size::fill()
                            })
                            .on_sized(on_item_sized)
                            .child(item)
                            .into()
                    }
                }
            })
            .collect::<Vec<Element>>();

        let (offset_x, offset_y) = match direction {
            Direction::Vertical => (
                corrected_scrolled_x,
                -(-corrected_scrolled_y - start_offset),
            ),
            Direction::Horizontal => (
                -(-corrected_scrolled_x - start_offset),
                corrected_scrolled_y,
            ),
        };

        let on_pointer_down = move |e: Event<PointerEventData>| {
            if drag_scrolling {
                let task_opt = *momentum_task.peek(); // extract before if-let to drop ReadRef
                if let Some(task) = task_opt {
                    // Cancel in-flight momentum. velocity_tracker is cleared so a short tap
                    // won't accumulate enough velocity to re-trigger momentum (< MIN_FLING_VELOCITY).
                    // drag_origin is still set below so a fast swipe in the opposite direction
                    // can immediately start a new gesture without requiring a second press.
                    task.cancel();
                    momentum_task.set(None);
                    suppress_next_press.set(true);
                    is_scrolling_state.set(false);
                }
                velocity_tracker.write().clear();
                drag_origin.set(Some(e.global_location()));
            }
        };

        rect()
            .width(layout.width.clone())
            .height(layout.height.clone())
            .a11y_id(a11y_id)
            .a11y_focusable(false)
            .a11y_role(AccessibilityRole::ScrollView)
            .a11y_builder(move |node| {
                node.set_scroll_x(corrected_scrolled_x as f64);
                node.set_scroll_y(corrected_scrolled_y as f64)
            })
            .scrollable(true)
            .on_wheel(on_wheel)
            .on_capture_global_pointer_press(on_capture_global_pointer_press)
            .on_mouse_move(on_mouse_move)
            .on_capture_global_pointer_move(on_capture_global_pointer_move)
            .on_key_down(on_key_down)
            .on_global_key_up(on_global_key_up)
            .on_global_key_down(on_global_key_down)
            .on_pointer_down(on_pointer_down)
            .child(
                rect()
                    .width(container_width)
                    .height(container_height)
                    .horizontal()
                    .child(
                        rect()
                            .direction(direction)
                            .width(content_width)
                            .height(content_height)
                            .offset_x(offset_x)
                            .offset_y(offset_y)
                            .overflow(Overflow::Clip)
                            .on_sized(move |e: Event<SizedEventData>| {
                                size.set_if_modified(e.clone())
                            })
                            .children(children),
                    )
                    .maybe_child(vertical_scrollbar_is_visible.then_some({
                        rect().child(ScrollBar {
                            theme: None,
                            clicking_scrollbar,
                            axis: Axis::Y,
                            offset: scrollbar_y,
                            size: Size::px(size.read().area.height()),
                            thumb: ScrollThumb {
                                theme: None,
                                clicking_scrollbar,
                                axis: Axis::Y,
                                size: scrollbar_height,
                            },
                        })
                    })),
            )
            .maybe_child(horizontal_scrollbar_is_visible.then_some({
                rect().child(ScrollBar {
                    theme: None,
                    clicking_scrollbar,
                    axis: Axis::X,
                    offset: scrollbar_x,
                    size: Size::px(size.read().area.width()),
                    thumb: ScrollThumb {
                        theme: None,
                        clicking_scrollbar,
                        axis: Axis::X,
                        size: scrollbar_width,
                    },
                })
            }))
    }

    fn render_key(&self) -> DiffKey {
        self.key.clone().or(self.default_key())
    }
}

fn get_render_range(
    viewport_size: f32,
    scroll_position: f32,
    item_size: f32,
    item_length: f32,
) -> Range<usize> {
    let render_index_start = (-scroll_position) / item_size;
    let potentially_visible_length = (viewport_size / item_size) + 1.0;
    let remaining_length = item_length - render_index_start;

    let render_index_end = if remaining_length <= potentially_visible_length {
        item_length
    } else {
        render_index_start + potentially_visible_length
    };

    render_index_start as usize..(render_index_end as usize)
}

/// Range of items that should be rendered for [`ItemSize::Dynamic`], along with the pixel offset
/// of the start of that range (the prefix sum of all the sizes before it).
struct VisibleRange {
    range: Range<usize>,
    start_offset: f32,
}

/// Computes [`VisibleRange`] for variable-size items using a prefix sum of `item_sizes` and a
/// binary search for the first visible item, mirroring [`get_render_range`] but for non-uniform
/// sizes.
fn get_dynamic_render_range(
    viewport_size: f32,
    scroll_position: f32,
    item_sizes: &[f32],
) -> VisibleRange {
    if item_sizes.is_empty() {
        return VisibleRange {
            range: 0..0,
            start_offset: 0.0,
        };
    }

    let scroll_offset = (-scroll_position).max(0.0);

    let mut prefix = Vec::with_capacity(item_sizes.len() + 1);
    prefix.push(0.0);
    let mut acc = 0.0;
    for &size in item_sizes {
        acc += size;
        prefix.push(acc);
    }

    // `prefix[i]` is the offset where item `i` *starts*, so the item containing `scroll_offset`
    // is the last one whose start is `<= scroll_offset` (i.e. one before the partition point).
    let start_index = prefix
        .partition_point(|&start| start <= scroll_offset)
        .saturating_sub(1)
        .min(item_sizes.len() - 1);
    let start_offset = prefix[start_index];

    let visible_end = scroll_offset + viewport_size;
    let mut end_index = start_index;
    let mut covered = prefix[start_index];
    while end_index < item_sizes.len() && covered < visible_end {
        covered += item_sizes[end_index];
        end_index += 1;
    }
    if end_index < item_sizes.len() {
        // Render one extra item past the viewport for smooth scrolling, like `get_render_range`.
        end_index += 1;
    }

    VisibleRange {
        range: start_index..end_index,
        start_offset,
    }
}
