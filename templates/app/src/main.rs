use gpui::{
    Context, IntoElement, MouseButton, Render, Window, WindowOptions, div, prelude::*, rgb,
};

struct Counter {
    count: usize,
}

impl Render for Counter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .bg(rgb(0x20242b))
            .text_color(rgb(0xffffff))
            .text_xl()
            .child(format!("Count: {}", self.count))
            .child(
                div()
                    .id("increment")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x4263eb))
                    .child("Increment")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.count += 1;
                            cx.notify();
                        }),
                    ),
            )
    }
}

fn main() {
    gpui_ce_platform::application().run(|cx| {
        cx.open_window(WindowOptions::default(), |_, cx| {
            cx.new(|_| Counter { count: 0 })
        })
        .expect("open window");
        cx.activate(true);
    });
}
