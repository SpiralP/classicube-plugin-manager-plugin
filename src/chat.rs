use classicube_helpers::{async_manager, chat};

pub async fn print_async<S: Into<String> + Send + 'static>(s: S) {
    async_manager::run_on_main_thread(async move {
        chat::print(s);
    })
    .await;
}
