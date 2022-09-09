/*
 *    Bot that acts as an extended inventory or bank for storing items
 *    Copyright (C) 2022  HumanoidSandvichDispenser
 *
 *    This program is free software: you can redistribute it and/or modify
 *    it under the terms of the GNU General Public License as published by
 *    the Free Software Foundation, either version 3 of the License, or
 *    (at your option) any later version.
 *
 *    This program is distributed in the hope that it will be useful,
 *    but WITHOUT ANY WARRANTY; without even the implied warranty of
 *    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *    GNU General Public License for more details.
 *
 *    You should have received a copy of the GNU General Public License
 *    along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

extern crate tokio;
extern crate veloren_client;
extern crate veloren_common;

use std::{env, sync::Arc, time::Duration};

use tokio::runtime::Runtime;
use veloren_client::{addr::ConnectionArgs, Client, Event, MarkerAllocator, WorldExt};
use veloren_common::{
    clock::Clock,
    comp::{self, ChatType},
    trade::{ReducedInventory, TradeAction},
    uid::{Uid, UidAllocator},
};

pub trait AliasOfUid {
    fn alias_of_uid(&self, uid: Uid) -> String;
}

impl AliasOfUid for Client {
    fn alias_of_uid(&self, uid: Uid) -> String {
        if let Some(player_info) = self.player_list().get(&uid) {
            return player_info.player_alias.clone();
        }
        String::from("Unknown")
    }
}

pub trait Until {
    fn until<T>(
        &mut self,
        clock: &mut Clock,
        predicate: T,
    ) -> Result<Vec<Event>, veloren_client::Error>
    where
        T: Fn(&mut Self) -> bool;
}

impl Until for Client {
    /// Blocks until the specified predicate is true or until the client errors.
    fn until<T>(
        &mut self,
        clock: &mut Clock,
        predicate: T,
    ) -> Result<Vec<Event>, veloren_client::Error>
    where
        T: Fn(&mut Self) -> bool,
    {
        loop {
            let tick = self.tick(comp::ControllerInputs::default(), clock.dt(), |_| {});
            if tick.is_ok() {
                if predicate(self) {
                    return tick;
                }
            } else {
                return tick;
            }
            self.cleanup();
            clock.tick();
        }
    }
}

fn until_create_character(
    client: &mut Client,
    clock: &mut Clock,
) -> Result<Vec<Event>, veloren_client::Error> {
    let body = comp::body::humanoid::Body {
        species: comp::body::humanoid::Species::Draugr,
        body_type: comp::body::humanoid::BodyType::Female,
        hair_style: 0,
        beard: 1,
        eyes: 0,
        accessory: 1,
        hair_color: 0,
        skin: 0,
        eye_color: 0,
    };

    client.create_character("Inventory Character".to_string(), None, None, body.into());

    println!("Created a new character.");

    client.until(clock, |c| !c.character_list().loading)
}

fn spawn_first_character(client: &mut Client, clock: &mut Clock) {
    // this is asynchronous. let's just keep loading.
    client.load_character_list();

    while client.presence().is_none() {
        if client
            .tick(comp::ControllerInputs::default(), clock.dt(), |_| ())
            .is_ok()
        {
            let character_list = client.character_list();
            if !character_list.loading {
                if client.character_list().characters.len() > 0 {
                    let character_ent = client.character_list().characters.first();
                    let character = character_ent.unwrap().character.clone();
                    client.request_character(character.id.unwrap());
                    println!("Requesting character {}", character.alias);
                } else {
                    // if we don't have a character, create one
                    until_create_character(client, clock)
                        .expect("Unable to create a new character");
                }
            }
        }

        client.cleanup();
        clock.tick();
    }
}

fn on_event(client: &mut Client, clock: &mut Clock) {
    // if client is not present (i.e. not spawned in yet), they should spawn
    // themselves NOW! ⛈
    if client.presence().is_none() {
        spawn_first_character(client, clock);
    }

    // check if we have an invite
    if let Some(last_invite) = client.invite() {
        let inviter_id = last_invite.0;
        if let Some(player_info) = client.player_list().get(&inviter_id) {
            if player_info.player_alias == env::var("TARGET_USERNAME").unwrap() {
                // we should the invite
                client.accept_invite();
            } else {
                // otherwise decline
                client.decline_invite();
            }
        }
    }

    // if we are currently in a trade, always accept
    if client.is_trading() {
        if let Some((_trade_id, pending_trade, _)) = client.pending_trade().clone() {
            if let Some(initiator_id) = pending_trade.parties.first() {
                if client.alias_of_uid(*initiator_id) == env::var("TARGET_USERNAME").unwrap() {
                    // keep accepting the trade if our intended user is the initiator
                    client.perform_trade_action(TradeAction::Accept(pending_trade.phase));

                    // get inventories and balance
                    let ecs = &client.state().ecs();
                    let inventories = ecs.read_component::<comp::Inventory>();
                    let get_inventory = |uid: Uid| {
                        if let Some(entity) = ecs
                            .read_resource::<UidAllocator>()
                            .retrieve_entity_internal(uid.0)
                        {
                            inventories.get(entity)
                        } else {
                            None
                        }
                    };

                    let mut party_inventories = [None, None];

                    for (i, party) in pending_trade.parties.iter().enumerate() {
                        println!("Fetching inventory {}", i);
                        match get_inventory(*party) {
                            Some(inventory) => {
                                party_inventories[i] = Some(ReducedInventory::from(inventory))
                            }
                            None => continue,
                        };
                    }
                }
            }
        }
    }
}

fn main() {
    println!("Starting veloren-inventory-bot...");
    let mut clock = Clock::new(Duration::from_secs_f64(1.0 / 16.0));
    let username = String::from(env::var("BOT_USERNAME").expect("$BOT_USERNAME is not set"));
    let password = String::from(env::var("BOT_PASSWORD").expect("$BOT_PASSWORD is not set"));
    env::var("TARGET_USERNAME").expect("$TARGET_USERNAME is not set");
    let server_addr = String::from("server.veloren.net:14004");

    let runtime = Arc::new(Runtime::new().unwrap());
    let runtime2 = Arc::clone(&runtime);

    let addr = ConnectionArgs::Tcp {
        hostname: server_addr.clone(),
        prefer_ipv6: false,
    };

    println!("Connecting to {}...", server_addr);

    let mut client = runtime
        .block_on(Client::new(addr, runtime2, &mut None))
        .expect("Unable to connect to server.");

    println!("Connected to {}", server_addr);
    println!("Server info: {:?}", client.server_info());
    println!("{} players online", client.player_list().len());

    println!("Logging in...");

    runtime
        .block_on(
            client.register(username.clone(), password.clone(), |provider| {
                // make sure that we are logging on using the correct auth server
                provider == "https://auth.veloren.net"
            }),
        )
        .expect("Unable to login.");

    println!("Logged in as {}", username);

    loop {
        let events = match client.tick(comp::ControllerInputs::default(), clock.dt(), |_| {}) {
            Ok(events) => {
                on_event(&mut client, &mut clock);
                events
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        };

        for event in events {
            match event {
                Event::Chat(message) => {
                    println!("{}", client.format_message(&message, true));

                    if let ChatType::Tell(from, _to) = message.chat_type {
                        let sender = client.alias_of_uid(from);
                        if sender == env::var("TARGET_USERNAME").unwrap() {
                            client.send_invite(from, comp::invite::InviteKind::Trade);
                        }
                    }
                }
                Event::Disconnect => {
                    println!("Disconnected.");
                }
                _ => {}
            }
        }

        client.cleanup();
        clock.tick();
    }
}
