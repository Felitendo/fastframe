use crate::{App, Error, Notification, Permission, worker};
pub(crate) struct Native;
impl worker::Backend for Native {
    type Delivery = Native;
    async fn show(&self, _: &App, _: &str, _: &Notification) -> Result<Native, Error> {
        Err(Error::Unsupported)
    }
    async fn permission(&self, _: &App, _: bool) -> Result<Permission, Error> {
        Err(Error::Unsupported)
    }
    async fn withdraw(&self, _: &App, _: &str) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}
impl worker::Delivery for Native {
    fn response(&mut self) -> worker::Response {
        Box::pin(async { crate::Event::Failed(Error::Unsupported) })
    }
    async fn update(&mut self, _: &Notification) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    async fn close(&mut self) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}
