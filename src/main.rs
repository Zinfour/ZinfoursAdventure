//! Example websocket server.
//!
//! Run the server with
//! ```not_rust
//! cargo run -p example-websockets --bin example-websockets
//! ```
//!
//! Run a browser client with
//! ```not_rust
//! firefox http://localhost:3000
//! ```
//!
//! Alternatively you can run the rust client (showing two
//! concurrent websocket connections being established) with
//! ```not_rust
//! cargo run -p example-websockets --bin example-client
//! ```
#![feature(let_chains)]
#![allow(unused_imports)]
// #![allow(dead_code)]
mod app_error;

use {
    app_error::AppError,
    axum::{
        body::Bytes,
        extract::{
            connect_info::ConnectInfo,
            ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
            Path, Query, Request, State,
        },
        response::{IntoResponse, Redirect, Response},
        routing::{any, get, post, put},
        Json, Router, ServiceExt,
    },
    axum_extra::TypedHeader,
    axum_server::tls_rustls::RustlsConfig,
    bincode::{deserialize, serialize},
    futures::{sink::SinkExt, stream::StreamExt},
    itertools::Itertools,
    maud::{html, Escaper, Markup, PreEscaped, Render, DOCTYPE},
    rand::{prelude::*, rng},
    redb::{
        backends::InMemoryBackend, Database, Key, MultimapTableDefinition, ReadableTable,
        ReadableTableMetadata, TableDefinition, TypeName, Value,
    },
    serde::{de::DeserializeOwned, Deserialize, Serialize},
    serde_inline_default::serde_inline_default,
    std::{
        any::type_name,
        cmp::Ordering,
        fmt::{Debug, Write},
        net::SocketAddr,
        ops::ControlFlow,
        path::PathBuf,
        str::FromStr,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    tokio::task::spawn_blocking,
    tower::Layer,
    tower_governor::{governor::GovernorConfigBuilder, GovernorLayer},
    tower_http::{
        normalize_path::NormalizePathLayer,
        services::ServeFile,
        trace::{DefaultMakeSpan, TraceLayer},
    },
    tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt},
    uuid::{uuid, Uuid},
};

const ADVENTURE_STEPS_TABLE: TableDefinition<Bincode<Uuid>, Bincode<AdventureStep>> =
    TableDefinition::new("adventure_steps");
// const USERS_TABLE: TableDefinition<Bincode<Uuid>, Bincode<User>> = TableDefinition::new("users");
const CHILDREN_TABLE: MultimapTableDefinition<Bincode<Uuid>, Bincode<Uuid>> =
    MultimapTableDefinition::new("children_links");
// const USER_HISTORY_TABLE: MultimapTableDefinition<Bincode<Uuid>, Bincode<UserInteraction>> =
//     MultimapTableDefinition::new("user_history");
// const LAST_SEEN_ACTIONS_TABLE: TableDefinition<Bincode<(Uuid, Uuid)>, Bincode<Vec<Uuid>>> =
//     TableDefinition::new("last_seen_actions");
const ADVENTURES_TABLE: TableDefinition<Bincode<Uuid>, Bincode<Adventure>> =
    TableDefinition::new("adventures");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AdventureStep {
    action: String,
    story: String,
    parent: Uuid,
    #[serde(default)]
    views: u64,
    #[serde(default)]
    created_by: Uuid,
    #[serde(default = "get_unix_time")]
    creation_time: u64,
    #[serde(default)]
    item_vector: Vec<f64>, // TODO: use this to embed/rank stories and actions
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct User {
    username: String,
    pw_hash: Vec<u8>,
    user_vector: Vec<f64>, // TODO: use this to embed/rank stories and actions
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, PartialOrd, Ord, Eq)]
enum UserInteractionData {
    SawStory(Uuid),
    SawNewActions(Vec<Uuid>),
    SawActions(Vec<Uuid>),
    PressedAction(Uuid),
    RefreshedActions(Vec<Uuid>),
    RefreshedStory(Uuid),
    AddedActionAndStory(Uuid),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, PartialOrd, Ord, Eq)]
struct UserInteraction {
    time: u64,
    interaction: UserInteractionData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Adventure {
    title: String,
    once_upon_a_time: String,
    #[serde(default)]
    created_by: Uuid,
    #[serde(default = "get_unix_time")]
    creation_time: u64,
    #[serde(default)]
    views: usize,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    item_vector: Vec<f64>, // TODO: use this to embed/rank stories and actions
}

#[derive(Clone, Debug)]
struct AppState {
    database: Arc<Database>,
}

// #[derive(Debug, Clone, Deserialize)]
// struct Credentials {
//     username: String,
//     password: String,
// }

#[derive(Debug)]
pub struct Bincode<T>(pub T);

impl<T> Value for Bincode<T>
where
    T: Debug + Serialize + for<'a> Deserialize<'a>,
{
    type SelfType<'a> = T
    where
        Self: 'a;

    type AsBytes<'a> = Vec<u8>
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        deserialize(data).unwrap()
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'a,
        Self: 'b,
    {
        serialize(value).unwrap()
    }

    fn type_name() -> TypeName {
        TypeName::new(&format!("Bincode<{}>", type_name::<T>()))
    }
}

impl<T> Key for Bincode<T>
where
    T: Debug + Serialize + DeserializeOwned + Ord,
{
    fn compare(data1: &[u8], data2: &[u8]) -> Ordering {
        Self::from_bytes(data1).cmp(&Self::from_bytes(data2))
    }
}

fn get_unix_time() -> u64 {
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    since_the_epoch.as_millis() as u64
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(20)
            .finish()
            .unwrap(),
    );

    let tls_config = RustlsConfig::from_pem_file(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("zinfour.com.pem"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("zinfour.com.key"),
    )
    .await
    .unwrap();

    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");

    let governor_limiter = governor_conf.limiter().clone();
    let interval = Duration::from_secs(60);
    // a separate background task to clean up
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        tracing::info!("rate limiting storage size: {}", governor_limiter.len());
        governor_limiter.retain_recent();
    });

    // TODO: default cache size is 1GB, maybe test if that is fine.
    // let database = Arc::new(Database::builder().create("./databases/zinfours_adventure_1.redb")?);
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    {
        let write_txs = database.begin_write().unwrap();
        {
            write_txs.open_table(ADVENTURES_TABLE).unwrap();
            // let mut users_table = write_txs.open_table(USERS_TABLE).unwrap();
            // users_table
            //     .insert(
            //         UUID_ZERO,
            //         User {
            //             username: "Guest".to_string(),
            //             user_vector: vec![],
            //             pw_hash: vec![],
            //         },
            //     )
            //     .unwrap();
            write_txs.open_table(ADVENTURE_STEPS_TABLE).unwrap();
            write_txs.open_multimap_table(CHILDREN_TABLE).unwrap();
        }
        write_txs.commit().unwrap();
    }
    // let db = database.clone();
    // std::thread::spawn::<_, Result<(), AppError>>(move || loop {
    //     std::thread::sleep(interval);
    //     let read_txs = db.begin_read()?;
    //     let adventures_stats = read_txs.open_table(ADVENTURES_TABLE)?.stats()?;
    //     let adventure_steps_stats = read_txs.open_table(ADVENTURE_STEPS_TABLE)?.stats()?;
    //     let children_stats = read_txs.open_multimap_table(CHILDREN_TABLE)?.stats()?;

    //     tracing::info!(
    //         "stats | cache: {:?}, adventures: {:?}, adventure steps: {:?}, children: {:?}",
    //         db.cache_stats(),
    //         adventures_stats,
    //         adventure_steps_stats,
    //         children_stats
    //     );
    // });

    // build our application with some routes
    let app = Router::new()
        .route("/", get(|| async { Redirect::permanent("/adventure") }))
        .route_service(
            "/adventure/directory",
            get(|| async { Redirect::permanent("/adventure") }),
        )
        // .route_service(
        //     "/adventure",
        //     ServeFile::new(assets_dir.join("directory.html")),
        // )
        .route("/adventure", get(directory))
        .route("/adventure/new_step", post(new_step))
        .route("/adventure/new", get(new_adventure))
        .route("/adventure/submit_adventure", post(submit_adventure))
        .route("/adventure/step/{key}", get(step))
        .route("/adventure/children/{key}", get(children))
        .route("/adventure/{key}", get(adventure_story))
        // .route("/adventure/create_guest", get(create_guest))
        .route_service("/styles.css", ServeFile::new(assets_dir.join("styles.css")))
        .route_service("/script.js", ServeFile::new(assets_dir.join("script.js")))
        .route_service(
            "/favicon.ico",
            ServeFile::new(assets_dir.join("favicon.ico")),
        )
        .route_service("/logo.png", ServeFile::new(assets_dir.join("logo.png")))
        // .route("/ws", any(ws_handler))
        .layer(GovernorLayer {
            config: governor_conf,
        })
        // logging so we can see whats going on
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
        .with_state(AppState { database });
    // run it with hyper

    let addr = SocketAddr::from(([192, 168, 1, 2], 443));

    // let addr = SocketAddr::from(([127, 0, 0, 1], 80));
    // let listener = tokio::net::TcpListener::bind("0.0.0.0:8800").await.unwrap();
    // let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    // tracing::debug!("listening on {}", listener.local_addr().unwrap());

    axum_server::bind_rustls(addr, tls_config)
        .serve(
            ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(
                NormalizePathLayer::trim_trailing_slash().layer(app),
            ),
        )
        .await
        .unwrap();
    // axum::serve(
    //     listener,
    //     app.into_make_service_with_connect_info::<SocketAddr>(),
    // )
    // .await
    // .unwrap();
}

// /// The handler for the HTTP request (this gets called when the HTTP request lands at the start
// /// of websocket negotiation). After this completes, the actual switching from HTTP to
// /// websocket protocol will occur.
// /// This is the last point where we can extract TCP/IP metadata such as IP address of the client
// /// as well as things from HTTP headers such as user-agent of the browser etc.
// async fn ws_handler(
//     ws: WebSocketUpgrade,
//     user_agent: Option<TypedHeader<headers::UserAgent>>,
//     ConnectInfo(addr): ConnectInfo<SocketAddr>,
// ) -> impl IntoResponse {
//     let user_agent = if let Some(TypedHeader(user_agent)) = user_agent {
//         user_agent.to_string()
//     } else {
//         String::from("Unknown browser")
//     };
//     println!("`{user_agent}` at {addr} connected.");
//     // finalize the upgrade process by returning upgrade callback.
//     // we can customize the callback by sending additional info such as address.
//     ws.on_upgrade(move |socket| handle_socket(socket, addr))
// }

// /// Actual websocket statemachine (one will be spawned per connection)
// async fn handle_socket(mut socket: WebSocket, who: SocketAddr) {
//     // send a ping (unsupported by some browsers) just to kick things off and get a response
//     if socket
//         .send(Message::Ping(Bytes::from_static(&[1, 2, 3])))
//         .await
//         .is_ok()
//     {
//         println!("Pinged {who}...");
//     } else {
//         println!("Could not send ping {who}!");
//         // no Error here since the only thing we can do is to close the connection.
//         // If we can not send messages, there is no way to salvage the statemachine anyway.
//         return;
//     }

//     // receive single message from a client (we can either receive or send with socket).
//     // this will likely be the Pong for our Ping or a hello message from client.
//     // waiting for message from a client will block this task, but will not block other client's
//     // connections.
//     if let Some(msg) = socket.recv().await {
//         if let Ok(msg) = msg {
//             if process_message(msg, who).is_break() {
//                 return;
//             }
//         } else {
//             println!("client {who} abruptly disconnected");
//             return;
//         }
//     }

//     // Since each client gets individual statemachine, we can pause handling
//     // when necessary to wait for some external event (in this case illustrated by sleeping).
//     // Waiting for this client to finish getting its greetings does not prevent other clients from
//     // connecting to server and receiving their greetings.
//     for i in 1..5 {
//         if socket
//             .send(Message::Text(format!("Hi {i} times!").into()))
//             .await
//             .is_err()
//         {
//             println!("client {who} abruptly disconnected");
//             return;
//         }
//         tokio::time::sleep(std::time::Duration::from_millis(100)).await;
//     }

//     // By splitting socket we can send and receive at the same time. In this example we will send
//     // unsolicited messages to client based on some sort of server's internal event (i.e .timer).
//     let (mut sender, mut receiver) = socket.split();

//     // Spawn a task that will push several messages to the client (does not matter what client does)
//     let mut send_task = tokio::spawn(async move {
//         // let n_msg = 20;
//         for i in 0.. {
//             // In case of any websocket error, we exit.
//             if sender
//                 .send(Message::Text(format!("Server message {i} ...").into()))
//                 .await
//                 .is_err()
//             {
//                 return i;
//             }

//             tokio::time::sleep(std::time::Duration::from_millis(100_000)).await;
//         }

//         println!("Sending close to {who}...");
//         if let Err(e) = sender
//             .send(Message::Close(Some(CloseFrame {
//                 code: axum::extract::ws::close_code::NORMAL,
//                 reason: Utf8Bytes::from_static("Goodbye"),
//             })))
//             .await
//         {
//             println!("Could not send Close due to {e}, probably it is ok?");
//         }
//         // n_msg
//         0
//     });

//     // This second task will receive messages from client and print them on server console
//     let mut recv_task = tokio::spawn(async move {
//         let mut cnt = 0;
//         while let Some(Ok(msg)) = receiver.next().await {
//             cnt += 1;
//             // print message and break if instructed to do so
//             if process_message(msg, who).is_break() {
//                 break;
//             }
//         }
//         cnt
//     });

//     // If any one of the tasks exit, abort the other.
//     tokio::select! {
//         rv_a = (&mut send_task) => {
//             match rv_a {
//                 Ok(a) => println!("{a} messages sent to {who}"),
//                 Err(a) => println!("Error sending messages {a:?}")
//             }
//             recv_task.abort();
//         },
//         rv_b = (&mut recv_task) => {
//             match rv_b {
//                 Ok(b) => println!("Received {b} messages"),
//                 Err(b) => println!("Error receiving messages {b:?}")
//             }
//             send_task.abort();
//         }
//     }

//     // returning from the handler closes the websocket connection
//     println!("Websocket context {who} destroyed");
// }

async fn new_adventure() -> Result<impl IntoResponse, AppError> {
    let document = html! {
        (DOCTYPE)
        head {
            title { "Zinfour's Adventure" }
            link rel="stylesheet" href="/styles.css";
            link rel="stylesheet" href="https://fonts.googleapis.com/css?family=Source Serif Pro";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            script src="/script.js" {};
        }
        body {
            #center-div {
                #title-header {
                    a href="/adventure" {
                        #logo-div {
                            img #logo src="/logo.png" alt="Zinfour's Adventure logo";
                        }
                    }
                    p contenteditable="plaintext-only" placeholder="Title..." { "" }
                }
                #messages-div {
                    #editing-messages-div {
                        ."story-msg" contenteditable="plaintext-only" placeholder="Once upon a time..." { "" }
                    }
                    #control-panel {
                        button #add-button title="Create" onclick="create_adventure()" { "Create" }
                    }
                }
            }
        }
    };

    Ok(document)
}

async fn submit_adventure(
    State(state): State<AppState>,
    Json(payload): Json<Adventure>,
) -> Result<Json<Uuid>, AppError> {
    println!("\n\n{:?}\n", payload);
    if payload.title.trim().is_empty() {
        Err(AppError::BadRequestError("Missing title.".to_string()))?;
    }
    if payload.title.len() > 50 {
        Err(AppError::BadRequestError("Title too long".to_string()))?;
    }
    if payload.once_upon_a_time.trim().is_empty() {
        Err(AppError::BadRequestError("Missing initial paragraph.".to_string()))?;
    }
    let adventure_key = {
        let database = state.database.clone();
        spawn_blocking(move || {
            let new_uuid = Uuid::new_v4();
            let write_txn = database.begin_write()?;
            {
                let mut adventures_table = write_txn.open_table(ADVENTURES_TABLE)?;
                adventures_table.insert(new_uuid, payload)?;
            }
            write_txn.commit()?;
            println!(">>> New adventure created: {}", new_uuid);
            Ok::<_, AppError>(new_uuid)
        })
        .await??
    };

    Ok(Json(adventure_key))
}

#[derive(Deserialize)]
struct AdventureStoryQuery {
    last_step: Option<Uuid>,
}

async fn adventure_story(
    Path(key): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<AdventureStoryQuery>,
) -> Result<Response, AppError> {
    let adventure_key = Uuid::from_str(&key).map_err(|e| e.to_string())?;
    let (adventure, adventure_steps) = {
        let database = state.database.clone();
        spawn_blocking(move || {
            let read_txn = database.begin_read()?;
            let adventure_steps_table = read_txn.open_table(ADVENTURE_STEPS_TABLE)?;
            let mut actions_and_stories = vec![];
            if let Some(mut current_uuid) = query.last_step {
                while let Some(v) = adventure_steps_table.get(current_uuid)? {
                    let v = v.value();
                    let v_parent = v.parent;
                    actions_and_stories.push((current_uuid, v));
                    current_uuid = v_parent;
                }
            }
            let adventures_table = read_txn.open_table(ADVENTURES_TABLE)?;
            let root_adventure = adventures_table
                .get(adventure_key)?
                .map(|v| v.value())
                .ok_or("couldn't find adventure")?;
            Ok::<_, AppError>((root_adventure, actions_and_stories))
        })
        .await??
    };

    let document = html! {
        (DOCTYPE)
        head {
            title { "Zinfour's Adventure" }
            link rel="stylesheet" href="/styles.css";
            link rel="stylesheet" href="https://fonts.googleapis.com/css?family=Source Serif Pro";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            script src="/script.js" {};
        }
        body {
            #center-div {
                #title-header {
                    a href="/adventure" {
                        #logo-div {
                            img #logo src="/logo.png" alt="Zinfour's Adventure logo";
                        }
                    }
                    p { (adventure.title) }
                }
                #messages-div {
                    #normal-messages-div {
                        ."story-msg" onclick={"go_back_to_origin()"} data-uuid={ (adventure_key) } { (adventure.once_upon_a_time) }
                        @for (i, (uuid, adventure_step)) in ((0..adventure_steps.len()).rev()).zip(adventure_steps.iter()).rev() {
                            ."action-msg" data-uuid={ (uuid) } { (adventure_step.action) }
                            ."story-msg" onclick={"go_back_to_story(" (i) ")"} data-uuid={ (uuid) } { (adventure_step.story) }
                        }
                        
                    }
                    #next-action-div {}
                    #editing-messages-div {}
                    #control-panel {
                        button #add-button title="Extend" onclick="add_button()" { "Extend" }
                        button #edit-button title="Edit" onclick="edit_button()" { "Edit" }
                        button #discard-button title="Discard Edits" onclick="discard_button()" { "Discard Edits" }
                        button #save-button title="Save Edits" onclick="save_button()" { "Save Edits" }
                    }
                }
            }
        }
    };

    // TODO: I don't like increasing viewcount like this because then we write to the database on every request.
    // Doing this in maybe another thread to reduce latency.
    let database = state.database.clone();
    spawn_blocking(move || {
        let mut write_txn = database.begin_write()?;
        write_txn.set_durability(redb::Durability::None);
        {
            let mut adventures_table = write_txn.open_table(ADVENTURES_TABLE)?;
            let mut adv = adventures_table
                .get(adventure_key)?
                .map(|z| z.value())
                .ok_or("no adventure key")?;
            adv.views += 1;
            adventures_table.insert(adventure_key, adv)?;
            let mut adventure_steps_table = write_txn.open_table(ADVENTURE_STEPS_TABLE)?;
            for (uuid, _) in adventure_steps {
                let mut step = adventure_steps_table
                    .get(uuid)?
                    .map(|z| z.value())
                    .ok_or("missing step")?;
                step.views += 1;
                adventure_steps_table.insert(uuid, step)?;
            }
        }
        write_txn.commit()?;
        Ok::<_, AppError>(())
    });

    Ok(document.into_response())
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    TopAllTime,
    TopThisYear,
    TopThisMonth,
    TopThisWeek,
    TopThisDay,
    Trending,
    Length,
    Random,
    New,
}

impl Render for SortBy {
    fn render_to(&self, output: &mut String) {
        let mut escaper = Escaper::new(output);
        write!(escaper, "{:?}", self).unwrap();
    }
}

#[derive(Deserialize)]
struct DirectoryQuery {
    sort_by: Option<SortBy>,
    page: Option<usize>,
}

async fn directory(
    State(state): State<AppState>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Markup, AppError> {
    let page = query.page.unwrap_or(1);
    let items_per_page = 10;
    let chosen_adventures: Vec<(Uuid, Adventure)> = {
        let database = state.database.clone();
        spawn_blocking(move || {
            let read_txn = database.begin_read()?;
            let adventures_table = read_txn.open_table(ADVENTURES_TABLE)?;
            let mut all_adventures = vec![];
            for adventure in adventures_table.iter()? {
                let (k, v) = adventure?;
                let adv_val = v.value();
                all_adventures.push((k.value(), adv_val));
            }
            drop(read_txn);
            
            let adventures = match query.sort_by {
                None | Some(SortBy::TopAllTime) => {
                    all_adventures
                        .into_iter()
                        .sorted_by_key(|(_, adv_val)| adv_val.views)
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::TopThisYear) => {
                    let unix_time = get_unix_time();
                    all_adventures
                        .into_iter()
                        .filter(|(_, adv_val)| {
                            unix_time.saturating_sub(adv_val.creation_time)
                                < 1000 * 60 * 60 * 24 * 365
                        })
                        .sorted_by_key(|(_, adv_val)| adv_val.views)
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::TopThisMonth) => {
                    let unix_time = get_unix_time();
                    all_adventures
                        .into_iter()
                        .filter(|(_, adv_val)| {
                            unix_time.saturating_sub(adv_val.creation_time)
                                < 1000 * 60 * 60 * 24 * 30
                        })
                        .sorted_by_key(|(_, adv_val)| adv_val.views)
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::TopThisWeek) => {
                    let unix_time = get_unix_time();
                    all_adventures
                        .into_iter()
                        .filter(|(_, adv_val)| {
                            unix_time.saturating_sub(adv_val.creation_time)
                                < 1000 * 60 * 60 * 24 * 7
                        })
                        .sorted_by_key(|(_, adv_val)| adv_val.views)
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::TopThisDay) => {
                    let unix_time = get_unix_time();
                    all_adventures
                        .into_iter()
                        .filter(|(_, adv_val)| {
                            unix_time.saturating_sub(adv_val.creation_time) < 1000 * 60 * 60 * 24
                        })
                        .sorted_by_key(|(_, adv_val)| adv_val.views)
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::Trending) => {
                    // Trending is sorted by views but with a halftime of 1 day.
                    let unix_time = get_unix_time();
                    all_adventures
                        .into_iter()
                        .sorted_by_key(|(_, adv_val)| {
                            (adv_val.views as f64
                                * (-(unix_time.saturating_sub(adv_val.creation_time) as f64
                                    / (1000 * 60 * 60 * 24) as f64))
                                    .exp2()) as u64
                        })
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::Length) => {
                    all_adventures
                        .into_iter()
                        .sorted_by_key(|(_, adv_val)| adv_val.length)
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::Random) => {
                    all_adventures.shuffle(&mut rng());
                    all_adventures
                        .into_iter()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
                Some(SortBy::New) => {
                    all_adventures
                        .into_iter()
                        .sorted_by_key(|(_, adv_val)| adv_val.creation_time)
                        .rev()
                        .skip(items_per_page * page.saturating_sub(1))
                        .take(items_per_page)
                        .collect()
                }
            };
            Ok::<_, AppError>(adventures)
        })
        .await??
    };

    let sort_by_options = vec![
        (SortBy::Trending, "Trending"),
        (SortBy::TopAllTime, "Top: All time"),
        (SortBy::TopThisYear, "Top: This year"),
        (SortBy::TopThisMonth, "Top: This month"),
        (SortBy::TopThisWeek, "Top: This week"),
        (SortBy::TopThisDay, "Top: Today"),
        (SortBy::Length, "Length"),
        (SortBy::Random, "Random"),
        (SortBy::New, "New"),
    ];

    let document = html! {
        (DOCTYPE)
        head {
            title { "Zinfour's Adventure" }
            link rel="stylesheet" href="/styles.css";
            link rel="stylesheet" href="https://fonts.googleapis.com/css?family=Source Serif Pro";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            script src="/script.js" {};
        }
        body {
            #center-div {
                #title-header {
                    a href="/adventure" {
                        #logo-div {
                            img #logo src="/logo.png" alt="Zinfour's Adventure logo";
                        }
                    }
                    p { "Zinfour's Adventure" }
                }
                #story-list {
                    #directory-settings {
                        select #sort-by onchange="document.location.href=\"/adventure?sort_by=\" + this.value" {
                            @for (sb, txt) in sort_by_options {
                                option value=(sb) selected[query.sort_by == Some(sb)] { (txt) }
                            }
                        }
                    }
                    #story-info-list {
                        @for (uuid, adventure) in chosen_adventures {
                            ."story-info" {
                                a href={ "/adventure/" (uuid) } { (adventure.title) }
                            }
                        }
                    }
                    #control-panel {
                        button #add-button title="Create Adventure" onclick="add_adventure_button()" { "Create Adventure" }
                    }
                }
            }
        }
    };
    Ok(document)
}

async fn new_step(
    State(state): State<AppState>,
    Json(payload): Json<AdventureStep>,
) -> Result<Json<Uuid>, AppError> {
    let new_step_uuid = Uuid::new_v4();
    if payload.action.len() > 100 {
        Err(AppError::BadRequestError("Too long action.".to_string()))?;
    }
    if payload.story.len() > 6000 {
        Err(AppError::BadRequestError("Too long story.".to_string()))?;
    }
    println!("{:?}", payload);
    {
        let database = state.database.clone();
        spawn_blocking(move || {
            let write_txn = database.begin_write()?;
            {
                let mut adventure_steps_table = write_txn.open_table(ADVENTURE_STEPS_TABLE)?;
                let adventures_table = write_txn.open_table(ADVENTURES_TABLE)?;
                let mut children_table = write_txn.open_multimap_table(CHILDREN_TABLE)?;

                if adventures_table.get(payload.parent)?.is_none()
                    && adventure_steps_table.get(payload.parent)?.is_none()
                {
                    Err("parent uuid is missing")?;
                }
                children_table.insert(payload.parent, new_step_uuid)?;
                adventure_steps_table.insert(new_step_uuid, payload)?;
            }
            write_txn.commit()?;
            Ok::<_, AppError>(())
        })
        .await??
    }
    Ok(Json(new_step_uuid))
}

async fn step(
    Path(key): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<AdventureStep>, AppError> {
    let key = Uuid::from_str(&key).map_err(|e| e.to_string())?;
    let step = {
        let database = state.database.clone();
        spawn_blocking(move || {
            let read_txn = database.begin_read()?;
            let adventure_steps_table = read_txn.open_table(ADVENTURE_STEPS_TABLE)?;
            let step = adventure_steps_table
                .get(key)?
                .map(|v| v.value())
                .ok_or("missing step uuid")?;
            Ok::<_, AppError>(step)
        })
        .await??
    };
    Ok(Json(step))
}

async fn children(
    Path(key): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<(Uuid, AdventureStep)>>, AppError> {
    let step_key = Uuid::from_str(&key).map_err(|e| e.to_string())?;
    let children = get_children(&state.database, step_key).await?;

    // TODO: I don't like increasing viewcount like this because then we write to the database on every request.
    // Doing this in maybe another thread to reduce latency.
    let database = state.database.clone();
    spawn_blocking(move || {
        let (adventure_steps, adventure_key) = {
            let read_txn = database.begin_read()?;
            let adventure_steps_table = read_txn.open_table(ADVENTURE_STEPS_TABLE)?;
            let mut actions_and_stories = vec![];
            let mut current_uuid = step_key;
            while let Some(v) = adventure_steps_table.get(current_uuid)? {
                let v = v.value();
                let v_parent = v.parent;
                actions_and_stories.push((current_uuid, v));
                current_uuid = v_parent;
            }
            Ok::<_, AppError>((actions_and_stories, current_uuid))
        }?;
        let mut write_txn = database.begin_write()?;
        write_txn.set_durability(redb::Durability::None);
        {
            let mut adventures_table = write_txn.open_table(ADVENTURES_TABLE)?;
            let mut adv = adventures_table
                .get(adventure_key)?
                .map(|z| z.value())
                .ok_or("no adventure key")?;
            adv.views += 1;
            adventures_table.insert(adventure_key, adv)?;
            let mut adventure_steps_table = write_txn.open_table(ADVENTURE_STEPS_TABLE)?;
            for (uuid, _) in adventure_steps {
                let mut step = adventure_steps_table
                    .get(uuid)?
                    .map(|z| z.value())
                    .ok_or("missing step")?;
                step.views += 1;
                adventure_steps_table.insert(uuid, step)?;
            }
        }
        write_txn.commit()?;
        Ok::<_, AppError>(())
    });
    Ok(Json(children))
}

async fn get_children(
    database: &Arc<Database>,
    uuid: Uuid,
) -> Result<Vec<(Uuid, AdventureStep)>, AppError> {
    let children = {
        let database = database.clone();
        spawn_blocking(move || {
            let read_txn = database.begin_read()?;
            let children_table = read_txn.open_multimap_table(CHILDREN_TABLE)?;
            let adventure_steps_table = read_txn.open_table(ADVENTURE_STEPS_TABLE)?;
            let mut all_children = vec![];
            let rng = &mut rng();
            for v in children_table.get(uuid)? {
                let uuid = v?.value();
                let step = adventure_steps_table
                    .get(uuid)?
                    .map(|v| v.value())
                    .ok_or("couln't find adventure step.")?;
                all_children.push((uuid, step));
            }
            let mut chosen_children = all_children
                .choose_multiple_weighted(rng, 3, |(_, step)| step.views as f64 + 1.0)?
                .cloned()
                .collect::<Vec<_>>();
            chosen_children.shuffle(rng);
            Ok::<_, AppError>(chosen_children)
        })
        .await??
    };
    Ok(children)
}

// /// helper to print contents of messages to stdout. Has special treatment for Close.
// fn process_message(msg: Message, who: SocketAddr) -> ControlFlow<(), ()> {
//     match msg {
//         Message::Text(t) => {
//             println!(">>> {who} sent str: {t:?}");
//         }
//         Message::Binary(d) => {
//             println!(">>> {} sent {} bytes: {:?}", who, d.len(), d);
//         }
//         Message::Close(c) => {
//             if let Some(cf) = c {
//                 println!(
//                     ">>> {} sent close with code {} and reason `{}`",
//                     who, cf.code, cf.reason
//                 );
//             } else {
//                 println!(">>> {who} somehow sent close message without CloseFrame");
//             }
//             return ControlFlow::Break(());
//         }

//         Message::Pong(v) => {
//             println!(">>> {who} sent pong with {v:?}");
//         }
//         // You should never need to manually handle Message::Ping, as axum's websocket library
//         // will do so for you automagically by replying with Pong and copying the v according to
//         // spec. But if you need the contents of the pings you can see them here.
//         Message::Ping(v) => {
//             println!(">>> {who} sent ping with {v:?}");
//         }
//     }
//     ControlFlow::Continue(())
// }
