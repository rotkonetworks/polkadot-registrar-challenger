use super::JsonResult;
use crate::primitives::IdentityContext;
use crate::Result;
use actix::prelude::*;
use actix_broker::BrokerSubscribe;
use std::{collections::HashMap, ops::Rem};

#[derive(Default)]
pub struct LookupServer {}

impl SystemService for LookupServer {}
impl Supervised for LookupServer {}

impl Actor for LookupServer {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        // TODO: Use arbiter instead?
        //self.subscribe_system_async::<AddIdentityState>(ctx);
    }
}
