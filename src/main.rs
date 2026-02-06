#![feature(trivial_bounds)]
#![feature(duration_constructors)]
#![deny(clippy::disallowed_methods)]
mod app_error;

use {
    app_error::AppError,
    argon2::{
        password_hash::{rand_core::OsRng, SaltString},
        Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    },
    async_trait::async_trait,
    axum::{
        extract::{
            Path, Query, Request, State,
        },
        http::{HeaderMap, HeaderValue, StatusCode},
        response::{Redirect},
        routing::{get, post},
        Form, Json, Router, ServiceExt,
    },
    axum_extra::{
        extract::{
            cookie::{Cookie, CookieJar},
        },
    },
    axum_login::{AuthManagerLayerBuilder, AuthSession, AuthUser, AuthnBackend, UserId},
    axum_server::tls_rustls::RustlsConfig,
    maud::{html, Escaper, Markup, Render, DOCTYPE},
    rand::{prelude::*, rng},
    serde::{Deserialize, Serialize},
    sqlx::{
        postgres::PgPoolOptions,
        types::Uuid,
        FromRow, PgPool, Pool, Postgres,
    },
    std::{
        fmt::{Debug, Write},
        fs::File,
        io::BufRead,
        net::SocketAddr,
        path::PathBuf,
        str::FromStr,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    tower::Layer,
    tower_governor::{governor::GovernorConfigBuilder, GovernorLayer},
    tower_http::{
        compression::CompressionLayer,
        normalize_path::NormalizePathLayer,
        services::ServeFile,
        trace::{DefaultMakeSpan, TraceLayer},
        CompressionLevel,
    },
    tower_sessions::{
        Expiry, SessionManagerLayer,
    },
    tower_sessions_sqlx_store::PostgresStore,
};
use dotenvy::dotenv;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, FromRow)]
struct AdventureStep {
    key: Uuid,
    action: String,
    story: String,
    parent: Uuid,
    views: i64,
    // like views but decays by 50% every day
    trending_views: i64,
    created_by: Vec<u8>, // UserIdent serialized by bincode because we can't go directly from postgres to UserIdent, see: https://github.com/launchbadge/sqlx/issues/514
    creation_time: i64,
    hidden: bool,
    item_vector: Vec<f32>, // TODO: use this to embed/rank stories and actions
    tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
enum UserIdent {
    Username(String),
    SessionId(Uuid),
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct User {
    username: String,
    pw_hash: String,
    user_vector: Vec<f32>, // TODO: use this to embed/rank stories and actions
    session_hash: Vec<u8>,
}


impl Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("username", &self.username)
            .field("pw_hash", &"_")
            .field("user_vector", &self.user_vector)
            .field("session_hash", &self.session_hash)
            .finish()
    }
}

impl AuthUser for User {
    type Id = String;

    fn id(&self) -> Self::Id {
        self.username.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        &self.session_hash
    }
}

#[derive(Clone, Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

#[async_trait]
impl AuthnBackend for AppState {
    type User = User;
    type Credentials = Credentials;
    type Error = AppError;

    async fn authenticate(
        &self,
        Credentials { username, password }: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let user_data = sqlx::query!("SELECT * FROM Users WHERE username = $1", username.clone())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(AppError::BadRequest("wrong credentials".to_string()))?;

        let parsed_hash = PasswordHash::new(&user_data.pw_hash)?;

        if Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
        {
            let session_hash = parsed_hash
                .hash
                .ok_or(AppError::InternalServerError("no hash found".to_string()))?
                .as_bytes()
                .to_vec();
            Ok(Some(User {
                username: user_data.username,
                pw_hash: user_data.pw_hash,
                user_vector: user_data.user_vector,
                session_hash,
            }))
        } else {
            Err(AppError::BadRequest("wrong credentials".to_string()))
        }
    }

    async fn get_user(&self, username: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        let user_data = sqlx::query!("SELECT * FROM Users WHERE username = $1", username.clone())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(AppError::BadRequest("wrong credentials".to_string()))?;

        let session_hash = PasswordHash::new(&user_data.pw_hash)?
            .hash
            .ok_or(AppError::InternalServerError("no hash found".to_string()))?
            .as_bytes()
            .to_vec();
        Ok(Some(User {
            username: user_data.username,
            pw_hash: user_data.pw_hash,
            user_vector: user_data.user_vector,
            session_hash,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, FromRow)]
struct Adventure {
    key: Uuid,
    title: String,
    once_upon_a_time: String,
    created_by: Vec<u8>, // UserIdent serialized by bincode because we can't go directly from postgres to UserIdent, see: https://github.com/launchbadge/sqlx/issues/514
    creation_time: i64,
    views: i64,
    trending_views: i64,
    length: i64,
    hidden: bool,
    item_vector: Vec<f32>, // TODO: use this to embed/rank stories and actions
    tags: Vec<String>,
}

#[derive(Clone, Debug)]
struct AppState {
    pool: PgPool,
    osu_osu_leaderboard: Arc<Vec<(usize, String, f32, usize)>>,
    osu_taiko_leaderboard: Arc<Vec<(usize, String, f32, usize)>>,
    osu_fruits_leaderboard: Arc<Vec<(usize, String, f32, usize)>>,
    osu_mania_leaderboard: Arc<Vec<(usize, String, f32, usize)>>,
}

fn get_unix_time() -> u64 {
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    since_the_epoch.as_millis() as u64
}

async fn connect_to_database() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("Missing DATABASE_URL.");

    PgPoolOptions::new()
        .max_connections(32)
        .connect(&database_url)
        .await
        .unwrap()
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    // for (key, value) in std::env::vars() {
    //     println!("{}: {}", key, value);
    // }
    // return;

    // tracing_subscriber::registry()
    //     .with(
    //         tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    //             format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
    //         }),
    //     )
    //     .with(tracing_subscriber::fmt::layer())
    //     .init();

    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(20)
            .burst_size(100)
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
        governor_limiter.retain_recent();
    });

    let pool = connect_to_database().await;

    // TODO: default cache size is 1GB, maybe test if that is fine.

    let app_state = AppState {
        pool: pool.clone(),
        osu_osu_leaderboard: Arc::new(read_osu_file("./osu_rankings/osu_ranking.txt")),
        osu_taiko_leaderboard: Arc::new(read_osu_file("./osu_rankings/taiko_ranking.txt")),
        osu_fruits_leaderboard: Arc::new(read_osu_file("./osu_rankings/catch_ranking.txt")),
        osu_mania_leaderboard: Arc::new(read_osu_file("./osu_rankings/mania_ranking.txt")),
    };


    let session_store = PostgresStore::new(pool);
    session_store.migrate().await.unwrap();

    let session_layer = SessionManagerLayer::new(session_store).with_expiry(Expiry::OnInactivity(
        tower_sessions::cookie::time::Duration::weeks(4 * 6),
    ));
    // Auth service.
    let auth_layer = AuthManagerLayerBuilder::new(app_state.clone(), session_layer).build();

    // build our application with some routes
    let app = Router::new()
        .route("/", get(homepage))
        .route("/osu", get(osu_page))
        .route_service(
            "/adventure/directory",
            get(|| async { Redirect::permanent("/adventure") }),
        )
        .route("/adventure", get(directory_page))
        .route("/adventure/about", get(about_page))
        .route("/adventure/new_step", post(new_step))
        .route_service(
            "/adventure/twitter_preview.png",
            ServeFile::new(assets_dir.join("twitter_preview.png")),
        )
        .route("/adventure/login", post(login))
        .route("/adventure/logout", get(logout))
        .route("/adventure/register", get(register_page))
        .route("/adventure/register_account", post(register))
        .route("/adventure/new", get(new_adventure_page))
        .route("/adventure/submit_adventure", post(submit_adventure))
        .route(
            "/adventure/children/{key}",
            get(choose_children_and_update_views),
        )
        .route("/adventure/{key}", get(adventure_story))
        .route_service("/styles.css", ServeFile::new(assets_dir.join("styles.css")))
        .route_service("/script.js", ServeFile::new(assets_dir.join("script.js")))
        .route_service(
            "/favicon.ico",
            ServeFile::new(assets_dir.join("favicon.ico")),
        )
        .route_service("/logo.png", ServeFile::new(assets_dir.join("logo.png")))
        .layer(GovernorLayer {
            config: governor_conf,
        })
        .layer(auth_layer)
        // .layer(session_layer)
        .with_state(app_state)
        // logging so we can see whats going on
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
        .layer(
            CompressionLayer::new()
                .gzip(true)
                .quality(CompressionLevel::Best),
        );


    let addr = SocketAddr::from(([192, 168, 1, 3], 443));

    axum_server::bind_rustls(addr, tls_config)
        .serve(
            ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(
                NormalizePathLayer::trim_trailing_slash().layer(app),
            ),
        )
        .await
        .unwrap();
}

fn read_osu_file(file_path: &str) -> Vec<(usize, String, f32, usize)> {
    let mut out = Vec::new();
    let file = File::open(file_path).unwrap();
    for (rank, line) in std::io::BufReader::new(file).lines().enumerate() {
        let tmp = line.unwrap();
        let row: [&str; 4] = tmp.split(",").collect::<Vec<_>>().try_into().unwrap();
        let (user_id, skill, _avg_rank, username): (
            usize,
            f32,
            f32,
            String,
        ) = (
            row[0].parse().unwrap(),
            row[1].parse().unwrap(),
            row[2].parse().unwrap(),
            row[3].parse().unwrap(),
        );
        out.push((rank + 1, username, skill, user_id))
    }
    out
}

fn header_and_sidebar(title: &str, editable: bool, user: &UserIdent) -> Markup {
    html! {
        #sidebar onclick="absorb_click(event)" {
            a #close-button onclick="close_sidebar(event)" { "×" }

            @if let UserIdent::Username(username) = user {
                a #username { (username) }
                a href="/adventure/logout" { "Logout" }
            } @else {
                form #login-form action="/adventure/login" method="post" {
                    label {
                        "Username:"
                        input type="text" name="username";
                    }
                    label {
                        "Password:"
                        input type="password" name="password" autocomplete="new-password";
                    }
                    input type="submit" value="Login";
                }
                a href="/adventure/register" { "Register a new account" }
            }
            hr;
            a href="/adventure/about" { "About" }
            a href="https://zinfour.bsky.social" { "Bluesky" }
            a href="https://twitter.com/zinfour_" { "Twitter" }
        }
        span #open-button onclick="open_sidebar(event)" {
            "≡"
        }
        #title-header {
            a href="/adventure" {
                #logo-div {
                    img #logo src="/logo.png" alt="Zinfour's Adventure Database logo";
                }
            }
            @if editable {
                p contenteditable="plaintext-only" placeholder=(title) { "" }
            } @else {
                p { (title) }
            }
        }
    }
}

fn adventure_head(adventure_title: Option<&str>) -> Markup {
    html! {
        head {
            @if let Some(adventure_title) = adventure_title {
                title { "Zinfour's Adventure Database | " (adventure_title) }
            } @else {
                title { "Zinfour's Adventure Database" }
            }
            link rel="stylesheet" href="/styles.css";
            link rel="preconnect" href="https://fonts.googleapis.com";
            link rel="preconnect" href="https://fonts.gstatic.com" crossorigin;
            link href="https://fonts.googleapis.com/css2?family=Source+Serif+4:ital,opsz,wght@0,8..60,200..900;1,8..60,200..900&display=swap" rel="stylesheet";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            script src="/script.js" {};

            link rel="icon" type="image/x-icon" href="/favicon.ico";

            meta name="twitter:card" content="summary_large_image";
            meta name="twitter:site" content="@zinfour_";
            meta name="twitter:creator" content="@zinfour_";
            meta name="twitter:title" content="Zinfour's Adventure Database";
            meta name="twitter:description" content="a place to read and create interactive fiction";
            meta name="twitter:image" content="https://zinfour.com/adventure/twitter_preview.png";
        }
    }
}

fn osu_head() -> Markup {
    html! {
        head {
            title { "Zinfour's osu! Leaderboard" }
            link rel="stylesheet" href="/styles.css";
            link rel="preconnect" href="https://fonts.googleapis.com";
            link rel="preconnect" href="https://fonts.gstatic.com" crossorigin;
            link href="https://fonts.googleapis.com/css2?family=Source+Serif+4:ital,opsz,wght@0,8..60,200..900;1,8..60,200..900&display=swap" rel="stylesheet";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            script src="/script.js" {};

            link rel="icon" type="image/x-icon" href="/favicon.ico";

            meta name="twitter:card" content="summary_large_image";
            meta name="twitter:site" content="@zinfour_";
            meta name="twitter:creator" content="@zinfour_";
            meta name="twitter:title" content="Zinfour's osu! Leaderboard";
        }
    }
}

fn notifications() -> Markup {
    html! {
        #notification-div {}
    }
}

async fn new_adventure_page(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
) -> Result<(CookieJar, Markup), AppError> {
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };
    let document = html! {
        (DOCTYPE)
        html lang="en" {
            (adventure_head(Some("New Adventure")))
            body {
                (header_and_sidebar("Title...", true, &user_ident))
                (notifications())
                #center-div {
                    #messages-div {
                        #next-action-div {}
                        hr;
                        #editing-messages-div {
                            ."story-msg" contenteditable="plaintext-only" placeholder="Once upon a time..." { "" }
                        }
                        #control-panel {
                            button #add-button title="Create" onclick="create_adventure()" { "Create" }
                        }
                    }
                }
            }
        }
    };

    Ok((jar, document))
}

#[derive(Debug, Deserialize)]
struct SubmitAdventureStruct {
    title: String,
    once_upon_a_time: String,
}

#[axum::debug_handler]
async fn submit_adventure(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
    State(state): State<AppState>,
    Json(payload): Json<SubmitAdventureStruct>,
) -> Result<(CookieJar, Json<Uuid>), AppError> {
    if payload.title.trim().is_empty() {
        return Err(AppError::BadRequest("Missing title.".to_string()));
    }
    if payload.title.len() > 50 {
        return Err(AppError::BadRequest("Too long title.".to_string()));
    }
    if payload.once_upon_a_time.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Missing initial paragraph.".to_string(),
        ));
    }
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };
    let adventure_key = Uuid::new_v4();

    sqlx::query!("INSERT INTO Adventures (key, title, once_upon_a_time, created_by, creation_time, views, trending_views, length, hidden, item_vector, tags)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        adventure_key,
        payload.title,
        payload.once_upon_a_time,
        bincode::serialize(&user_ident)?,
        get_unix_time() as i64,
        0,
        0,
        0,
        false,
        &Vec::<f32>::new(),
        &Vec::<String>::new()
    )
        .execute(&state.pool)
        .await?;
    println!(">>> New adventure created: {}", adventure_key);
    Ok((jar, Json(adventure_key)))
}

#[derive(Deserialize)]
struct AdventureStoryQuery {
    last_step: Option<Uuid>,
}

async fn adventure_story(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
    Path(key): Path<String>,
    State(state): State<AppState>,
    Query(query): Query<AdventureStoryQuery>,
) -> Result<(CookieJar, Markup), AppError> {
    let adventure_key = Uuid::from_str(&key).map_err(|e| e.to_string())?;
    let mut adventure_steps = vec![];
    if let Some(mut current_uuid) = query.last_step {
        // TODO: replace this with a recursive query?
        while let Some(v) = sqlx::query_as!(
            AdventureStep,
            "SELECT * FROM AdventureSteps WHERE key = $1",
            &current_uuid
        )
        .fetch_optional(&state.pool)
        .await?
        {
            let v_parent = v.parent;
            adventure_steps.push((current_uuid, v));
            current_uuid = v_parent;
        }
    }
    let adventure = sqlx::query_as!(Adventure, "SELECT * FROM Adventures WHERE key = $1", &adventure_key)
        .fetch_optional(&state.pool)
        .await?
        .ok_or("couldn't find adventure")?;

    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };

    let document = html! {
        (DOCTYPE)
        html lang="en" {
            (adventure_head(Some(&adventure.title)))
            body {
                (header_and_sidebar(&adventure.title, false, &user_ident))
                (notifications())
                #center-div {
                    #messages-div {
                        #normal-messages-div {
                            ."story-msg" onclick={"go_back_to_origin()"} data-uuid={ (adventure_key) } { (adventure.once_upon_a_time) }
                            @for (i, (uuid, adventure_step)) in ((0..adventure_steps.len()).rev()).zip(adventure_steps.iter()).rev() {
                                ."action-msg" data-uuid={ (uuid) } { (adventure_step.action) }
                                ."msg-wrapper" data-uuid={ (uuid) } {
                                    ."story-msg" {
                                        (adventure_step.story)
                                    }
                                    ."nav-dock" {
                                        // "go back, link, refresh/randomize, extend"
                                        @if bincode::serialize(&user_ident)? == adventure_step.created_by {
                                            a onclick={"delete_adventure_step(" (i) "," (uuid) ")"} { "🗑️" }
                                        }
                                        a onclick={"link_to_adventure_step(\"" (uuid) "\")"} { "🔗" }
                                        a onclick={"go_back_to_story(" (i) ")"} { "⤒" }
                                        a onclick={"add_button(" (i) ")"} { "➕" }
                                    }
                                }
                            }
                        }
                        #next-action-div {
                            @for (i, step) in choose_children(&state.pool, query.last_step.unwrap_or(adventure_key))
                                .await?
                                .into_iter()
                                .enumerate()
                            {
                                ."action-msg" onclick={"choose_action(" (i) ")"} data-storytext=(step.story) data-uuid=(step.key) { (step.action) }
                            }
                        }
                        #editing-messages-div {}
                        #control-panel {
                            button #add-button title="Extend" onclick="add_button()" { "Extend" }
                            button #discard-button title="Discard" onclick="discard_button()" disabled { "Discard" }
                            button #save-button title="Save" onclick="save_button()" disabled { "Save" }
                        }
                    }
                }
            }
        }
    };

    // TODO: I don't like increasing viewcount like this because then we write to the database on every request.
    // Doing this in maybe another thread to reduce latency.
    sqlx::query!(
        "UPDATE Adventures SET views = views + 1 WHERE key = $1",
        &adventure_key
    )
    .execute(&state.pool)
    .await?;

    for (uuid, _) in adventure_steps {
        sqlx::query!(
            "UPDATE AdventureSteps
             SET views = views + 1
             WHERE key = $1",
            &uuid
        )
        .execute(&state.pool)
        .await?;
    }

    Ok((jar, document))
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum DirectorySortBy {
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

impl Render for DirectorySortBy {
    fn render_to(&self, output: &mut String) {
        let mut escaper = Escaper::new(output);
        write!(escaper, "{:?}", self).unwrap();
    }
}

#[derive(Deserialize)]
struct DirectoryQuery {
    sort_by: Option<DirectorySortBy>,
    page: Option<i64>,
}

fn directory_query_to_url(sort_by: Option<DirectorySortBy>, page: Option<i64>) -> String {
    match (sort_by, page) {
        (None, None) => format!("/adventure"),
        (None, Some(page)) => format!("/adventure?page={}", page),
        (Some(sort_by), None) => format!("/adventure?sort_by={:?}", sort_by),
        (Some(sort_by), Some(page)) => format!("/adventure?sort_by={:?}&page={}", sort_by, page),
    }
}

async fn directory_page(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
    State(state): State<AppState>,
    Query(query): Query<DirectoryQuery>,
) -> Result<(CookieJar, Markup), AppError> {
    let current_page = query.page.unwrap_or(1);
    let items_per_page = 15;

    let number_of_pages: i64 = sqlx::query_scalar!(r#"SELECT COUNT(*) as "!" FROM Adventures"#)
        .fetch_one(&state.pool)
        .await?
        .ok_or(AppError::InternalServerError(
            "query scalar failed".to_string(),
        ))?;

    let adventures = match query.sort_by {
        None | Some(DirectorySortBy::TopAllTime) => {
            sqlx::query_as!(
                Adventure,
                "SELECT * FROM Adventures WHERE hidden = false ORDER BY views DESC LIMIT $1 OFFSET $2",
                items_per_page as i64,
                items_per_page * current_page.saturating_sub(1) as i64
            )
            .fetch_all(&state.pool)
            .await?
        }
        Some(DirectorySortBy::TopThisYear) => {
            let unix_time = get_unix_time();

            sqlx::query_as!(
                Adventure,
                "SELECT * FROM Adventures WHERE hidden = false and creation_time >= $1 ORDER BY views DESC LIMIT $2 OFFSET $3"
                , (unix_time - 1000 * 60 * 60 * 24 * 365) as i64
                , items_per_page as i64
                , items_per_page * current_page.saturating_sub(1) as i64)
                .fetch_all(&state.pool)
                .await?
        }
        Some(DirectorySortBy::TopThisMonth) => {
            let unix_time = get_unix_time();
            sqlx::query_as!(Adventure, 
                "SELECT * FROM Adventures WHERE hidden = false and creation_time >= $1 ORDER BY views DESC LIMIT $2 OFFSET $3",
            
                (unix_time - 1000 * 60 * 60 * 24 * 30) as i64, 
                items_per_page as i64, 
                items_per_page * current_page.saturating_sub(1) as i64)
                .fetch_all(&state.pool)
                .await?
        }
        Some(DirectorySortBy::TopThisWeek) => {
            let unix_time = get_unix_time();
            sqlx::query_as!(Adventure,
                "SELECT * FROM Adventures WHERE hidden = false and creation_time >= $1 ORDER BY views DESC LIMIT $2 OFFSET $3",
                (unix_time - 1000 * 60 * 60 * 24 * 7) as i64, 
                items_per_page as i64, 
                items_per_page * current_page.saturating_sub(1) as i64)
                .fetch_all(&state.pool)
                .await?
        }
        Some(DirectorySortBy::TopThisDay) => {
            let unix_time = get_unix_time();
            sqlx::query_as!(Adventure,
                "SELECT * FROM Adventures WHERE hidden = false and creation_time >= $1 ORDER BY views DESC LIMIT $2 OFFSET $3",
                (unix_time - 1000 * 60 * 60 * 24) as i64, 
                items_per_page as i64, 
                items_per_page * current_page.saturating_sub(1) as i64)
                .fetch_all(&state.pool)
                .await?
        }
        Some(DirectorySortBy::Trending) => {
            sqlx::query_as!(Adventure,
                "SELECT * FROM Adventures WHERE hidden = false ORDER BY trending_views DESC LIMIT $1 OFFSET $2",
                items_per_page as i64,
                items_per_page * current_page.saturating_sub(1) as i64)
            .fetch_all(&state.pool)
            .await?
        }
        Some(DirectorySortBy::Length) => {
            sqlx::query_as!(Adventure,
                "SELECT * FROM Adventures WHERE hidden = false ORDER BY length DESC LIMIT $1 OFFSET $2",
                items_per_page as i64,
                items_per_page * current_page.saturating_sub(1) as i64)
            .fetch_all(&state.pool)
            .await?
        }
        Some(DirectorySortBy::Random) => {
            sqlx::query_as!(Adventure,
                "SELECT * FROM Adventures WHERE hidden = false ORDER BY RANDOM() LIMIT $1 OFFSET $2",
                items_per_page as i64,
                items_per_page * current_page.saturating_sub(1) as i64)
            .fetch_all(&state.pool)
            .await?
        }
        Some(DirectorySortBy::New) => {
            sqlx::query_as!(
                Adventure,
                "SELECT * FROM Adventures WHERE hidden = false ORDER BY creation_time DESC LIMIT $1 OFFSET $2",
                items_per_page as i64,
                items_per_page * current_page.saturating_sub(1) as i64
            )
            .fetch_all(&state.pool)
            .await?
        }
    };

    let sort_by_options = vec![
        (DirectorySortBy::Trending, "Trending"),
        (DirectorySortBy::TopAllTime, "Top: All time"),
        (DirectorySortBy::TopThisYear, "Top: This year"),
        (DirectorySortBy::TopThisMonth, "Top: This month"),
        (DirectorySortBy::TopThisWeek, "Top: This week"),
        (DirectorySortBy::TopThisDay, "Top: Today"),
        (DirectorySortBy::Length, "Length"),
        (DirectorySortBy::Random, "Random"),
        (DirectorySortBy::New, "New"),
    ];

    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };

    let document = html! {
        (DOCTYPE)
        html lang="en" {
            (adventure_head(None))
            body {
                (header_and_sidebar("Zinfour's Adventure Database", false, &user_ident))
                (notifications())
                #center-div {
                    #story-list {
                        #directory-settings {
                            select #sort-by onchange="document.location.href=\"/adventure?sort_by=\" + this.value" {
                                @for (sb, txt) in sort_by_options {
                                    option value=(sb) selected[query.sort_by == Some(sb)] { (txt) }
                                }
                            }
                            #page-nav {
                                @if current_page > 1 {
                                    a href=(directory_query_to_url(query.sort_by, Some(1))) { (1) }
                                }
                                @if current_page > 3 {
                                    a { "..." }
                                }
                                @if current_page > 2 {
                                    a href=(directory_query_to_url(query.sort_by, Some(current_page-1))) { (current_page-1) }
                                }
                                a href=(directory_query_to_url(query.sort_by, Some(current_page))) { (current_page) }
                                @if number_of_pages.saturating_sub(current_page) > 1 {
                                    a href=(directory_query_to_url(query.sort_by, Some(current_page+1))) { (current_page+1) }
                                }
                                @if number_of_pages.saturating_sub(current_page) > 2 {
                                    a { "..." }
                                }
                                @if number_of_pages.saturating_sub(current_page) > 0 {
                                    a href=(directory_query_to_url(query.sort_by, Some(number_of_pages))) { (number_of_pages) }
                                }
                            }
                        }
                        #story-info-list {
                            @for adventure in adventures {
                                ."story-info" {
                                    a href={ "/adventure/" (adventure.key) } { (adventure.title) }
                                }
                            }
                        }
                        #control-panel {
                            button #add-button title="Create Adventure" onclick="add_adventure_button()" { "Create Adventure" }
                        }
                    }
                }
            }
        }
    };
    Ok((jar, document))
}

async fn about_page(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
) -> Result<(CookieJar, Markup), AppError> {
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };

    let document = html! {
        (DOCTYPE)
        html lang="en" {
            (adventure_head(Some("About")))
            body {
                (header_and_sidebar("Zinfour's Adventure Database", false, &user_ident))
                #center-div {
                    img #logo src="/logo.png" alt="Zinfour's Adventure Database logo";
                    #about-text {
                        "Zinfour's Adventure Database is a place to explore and create Choose-Your-Own-Adventures, aka CYOAs. It's still very much work in progress so if you have any questions or feature requests don't hesitate to ping me at "
                        a href="https://twitter.com/zinfour_" { "Twitter" }
                        "/"
                        a href="https://zinfour.bsky.social" { "Bluesky" }
                        ". I hope people have fun reading and writing adventures. ^.^"
                    }
                }
            }
        }
    };
    Ok((jar, document))
}

async fn register_page(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
) -> Result<(CookieJar, Markup), AppError> {
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };
    let document = html! {
        (DOCTYPE)
        html lang="en" {
            (adventure_head(Some("Register")))
            body {
                (header_and_sidebar("Zinfour's Adventure Database", false, &user_ident))
                (notifications())
                #center-div {
                    form #register-form action="/adventure/register_account" method="post" {
                        label {
                            "Username:"
                            input type="text" name="username";
                        }
                        label {
                            "Password:"
                            input type="password" name="password" autocomplete="new-password";
                        }
                        input type="submit" value="Create Account";
                    }
                }
            }
        }
    };
    Ok((jar, document))
}

#[derive(Deserialize)]
struct RegisterStruct {
    username: String,
    password: String,
}

// Custom Debug implentation so that we never print the password.
impl Debug for RegisterStruct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterStruct")
            .field("username", &self.username)
            .field("password", &"_")
            .finish()
    }
}

async fn register(
    State(state): State<AppState>,
    Form(register_struct): Form<RegisterStruct>,
) -> Result<Redirect, AppError> {
    let salt = SaltString::generate(&mut OsRng);

    // Hash password to PHC string ($argon2id$v=19$...)
    let password_hash = Argon2::default()
        .hash_password(register_struct.password.as_bytes(), &salt)?
        .to_string();

    // TODO: make sure the schema has `CREATE UNIQUE INDEX idx_users_username ON Users(username);`
    let result = sqlx::query!(
        "INSERT INTO Users (username, pw_hash, user_vector)
         VALUES ($1, $2, $3)",
        &register_struct.username,
        password_hash,
        &Vec::<f32>::new()
    )
    .execute(&state.pool)
    .await;

    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(AppError::BadRequest(
                "Username is already taken.".to_string(),
            ));
        }
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    Ok(Redirect::to("/adventure"))
}

async fn login(
    mut jar: CookieJar,
    mut auth_session: AuthSession<AppState>,
    headers: HeaderMap,
    State(state): State<AppState>,
    Form(creds): Form<Credentials>,
) -> Result<(CookieJar, Redirect), AppError> {
    let user = match auth_session.authenticate(creds.clone()).await? {
        Some(user) => user,
        None => Err(AppError::BadRequest(
            "Wrong password or username".to_string(),
        ))?,
    };

    auth_session.login(&user).await?;

    if let Some(cookie) = jar.get("session-id") {
        transfer_from_session_id_user_to_auth_user(
            state,
            Uuid::from_str(cookie.value())?,
            user.username,
        )
        .await?;
        jar = jar.remove("session-id");
    }

    Ok((
        jar,
        Redirect::to(
            headers
                .get("referer")
                .unwrap_or(&HeaderValue::from_static("/adventure"))
                .to_str()?,
        ),
    ))
}

async fn logout(
    mut auth_session: AuthSession<AppState>,
    headers: HeaderMap,
) -> Result<Redirect, AppError> {
    auth_session.logout().await?;
    Ok(Redirect::to(
        headers
            .get("referer")
            .unwrap_or(&HeaderValue::from_static("/adventure"))
            .to_str()?,
    ))
}

#[derive(Debug, Deserialize)]
struct NewStepStruct {
    action: String,
    story: String,
    parent: Uuid,
}

fn get_or_create_session_id(mut jar: CookieJar) -> (CookieJar, Cookie<'static>) {
    if jar.get("session-id").is_none() {
        let salt = Uuid::new_v4();
        jar = jar.add(Cookie::new("session-id", salt.to_string()));
    }
    let c = jar.get("session-id").unwrap().clone();
    (jar, c)
}

#[axum::debug_handler]
async fn new_step(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
    State(state): State<AppState>,
    Json(payload): Json<NewStepStruct>,
) -> Result<(CookieJar, Json<Uuid>), AppError> {
    if payload.action.is_empty() {
        return Err(AppError::BadRequest("Too short action.".to_string()));
    }
    if payload.action.len() > 100 {
        return Err(AppError::BadRequest("Too long action.".to_string()));
    }
    if payload.story.is_empty() {
        return Err(AppError::BadRequest("Too short story.".to_string()));
    }
    if payload.story.len() > 6000 {
        return Err(AppError::BadRequest("Too long story.".to_string()));
    }
    let new_step_uuid = Uuid::new_v4();
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };

    let mut tx = state.pool.begin().await?;

    if sqlx::query_as!(AdventureStep, "SELECT * FROM AdventureSteps WHERE key = $1", &payload.parent)
        .fetch_optional(&mut *tx)
        .await?
        .is_none()
        && sqlx::query_as!(
                Adventure,
                "SELECT * FROM Adventures WHERE key = $1",
                &payload.parent
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_none()
    {
        Err("parent uuid is missing")?;
    }

    sqlx::query!("
            INSERT INTO AdventureSteps (key, action, story, parent, views, trending_views, created_by, creation_time, hidden, item_vector, tags)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ",
        new_step_uuid,
        payload.action,
        payload.story,
        payload.parent,
        0,
        0,
        bincode::serialize(&user_ident)?,
        get_unix_time() as i64,
        false,
        &Vec::<f32>::new(),
        &Vec::<String>::new()
        )
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok((jar, Json(new_step_uuid)))
}

#[derive(Debug, Serialize)]
struct ChildStepResponse {
    story: String,
    action: String,
}

async fn choose_children_and_update_views(
    Path(key): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<(Uuid, ChildStepResponse)>>, AppError> {
    let step_key = Uuid::from_str(&key).map_err(|e| e.to_string())?;
    let children = choose_children(&state.pool, step_key)
        .await?
        .into_iter()
        .map(|adventure_step| {
            (
                adventure_step.key,
                ChildStepResponse {
                    story: adventure_step.story,
                    action: adventure_step.action,
                },
            )
        })
        .collect::<Vec<_>>();

    // TODO: I don't like increasing viewcount like this because then we write to the database on every request.
    // Consider doing this in another thread to reduce latency?
    let (adventure_steps, adventure_key) = {
        let mut actions_and_stories = vec![];
        let mut current_key = step_key;

        while let Some(v) =
            sqlx::query_as!(AdventureStep, "SELECT * FROM AdventureSteps WHERE key = $1", &current_key)
                .fetch_optional(&state.pool)
                .await?
        {
            let v_parent = v.parent;
            actions_and_stories.push(current_key);
            current_key = v_parent;
        }
        (actions_and_stories, current_key)
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query!(
        "UPDATE Adventures
         SET views = views + 1
         WHERE key = $1",
        &adventure_key
    )
    .execute(&mut *tx)
    .await?;
    for key in adventure_steps {
        sqlx::query!(
            "UPDATE AdventureSteps
            SET views = views + 1
            WHERE key = $1",
            &key
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(children))
}

async fn choose_children(
    pool: &Pool<Postgres>,
    uuid: Uuid,
) -> Result<Vec<AdventureStep>, AppError> {
    let all_children = sqlx::query_as!(
        AdventureStep, 
        "SELECT * FROM AdventureSteps WHERE parent = $1 and hidden = false",
        &uuid
    )
    .fetch_all(pool)
    .await?;

    let rng = &mut rng();
    let chosen_children = all_children
        .choose_multiple_weighted(rng, 3, |step| step.views as f64 + 1.0)?
        .cloned()
        .collect::<Vec<_>>();
    Ok(chosen_children)
}

async fn moderation(
    mut jar: CookieJar,
    auth_session: AuthSession<AppState>,
) -> Result<(CookieJar, Markup), AppError> {
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };
    if let UserIdent::Username(username) = &user_ident
        && username == "zinfour"
    {
        let document = html! {
            (DOCTYPE)
            html lang="en" {
                (adventure_head(Some("Moderation")))
                body {
                    (header_and_sidebar("Zinfour's Adventure Database", false, &user_ident))
                    (notifications())
                    #center-div {
                        #moderation-div {
                            form action="/adventure/moderation/hide_uuid" method="post" {
                                label { "hide uuid:" }
                                input type="text" name="uuid";
                                input type="submit" value="Submit";
                            }
                            form action="/adventure/moderation/delete_uuid" method="post" {
                                label { "delete uuid:" }
                                input type="text" name="uuid";
                                input type="submit" value="Submit";
                            }
                        }
                    }
                }
            }
        };
        Ok((jar, document))
    } else {
        Err(AppError::BadRequest("only zinfour allowed".to_string()))
    }
}

#[derive(Deserialize)]
struct UuidStruct {
    uuid: Uuid,
}

async fn hide_uuid(
    mut jar: CookieJar,
    State(state): State<AppState>,
    Form(uuid): Form<UuidStruct>,
    auth_session: AuthSession<AppState>,
) -> Result<(CookieJar, StatusCode), AppError> {
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };
    if let UserIdent::Username(username) = &user_ident
        && username == "zinfour"
    {
        let mut tx = state.pool.begin().await?;

        sqlx::query!(
            "UPDATE Adventures
            SET hidden = true
            WHERE key = $1",
            &uuid.uuid
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE AdventureSteps
            SET hidden = true
            WHERE key = $1",
            &uuid.uuid
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok((jar, StatusCode::OK))
    } else {
        Err(AppError::BadRequest("only zinfour allowed".to_string()))
    }
}

async fn delete_uuid(
    mut jar: CookieJar,
    State(state): State<AppState>,
    Form(uuid): Form<UuidStruct>,
    auth_session: AuthSession<AppState>,
) -> Result<(CookieJar, StatusCode), AppError> {
    let user_ident = match auth_session.user {
        Some(user) => UserIdent::Username(user.username),
        None => {
            let (new_jar, c) = get_or_create_session_id(jar);
            jar = new_jar;
            UserIdent::SessionId(Uuid::from_str(c.value())?)
        }
    };
    if let UserIdent::Username(username) = &user_ident
        && username == "zinfour"
    {
        let mut tx = state.pool.begin().await?;
        sqlx::query!(
            "DELETE FROM Adventures
            WHERE key = $1",
            &uuid.uuid
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM AdventureSteps
            WHERE key = $1",
            &uuid.uuid
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok((jar, StatusCode::OK))
    } else {
        Err(AppError::BadRequest("only zinfour allowed".to_string()))
    }
}

async fn transfer_from_session_id_user_to_auth_user(
    state: AppState,
    session_id: Uuid,
    username: String,
) -> Result<StatusCode, AppError> {
    let mut tx = state.pool.begin().await?;

    // reassign creator of adventures
    sqlx::query!(
        "UPDATE Adventures
        SET created_by = $1
        WHERE created_by = $2",
        bincode::serialize(&UserIdent::Username(username.clone()))?,
        bincode::serialize(&UserIdent::SessionId(session_id))?
    )
    .execute(&mut *tx)
    .await?;

    // reassign creator of adventure steps
    sqlx::query!(
        "UPDATE AdventureSteps
        SET created_by = $1
        WHERE created_by = $2",
        bincode::serialize(&UserIdent::Username(username.clone()))?,
        bincode::serialize(&UserIdent::SessionId(session_id))?,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(StatusCode::OK)
}

fn osu_query_to_url(page: Option<usize>, search: Option<&String>, mode: Option<OsuMode>) -> String {
    let mut out = "/osu".to_string();
    let mut first = true;
    if let Some(page) = page {
        if first {
            out.push('?');
        } else {
            out.push('&');
        }
        out.push_str(&format!("page={}", page));
        first = false;
    }
    if let Some(search) = search {
        if first {
            out.push('?');
        } else {
            out.push('&');
        }
        out.push_str(&format!("search={}", search));
        first = false;
    }
    if let Some(mode) = mode {
        if first {
            out.push('?');
        } else {
            out.push('&');
        }
        out.push_str(&format!("mode={:?}", mode));
    }
    out
}

#[derive(Deserialize)]
struct OsuQuery {
    page: Option<usize>,
    search: Option<String>,
    mode: Option<OsuMode>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
enum OsuMode {
    Osu,
    Taiko,
    Fruits,
    Mania,
}

impl Render for OsuMode {
    fn render_to(&self, output: &mut String) {
        let mut escaper = Escaper::new(output);
        write!(escaper, "{:?}", self).unwrap();
    }
}

#[axum::debug_handler]
async fn osu_page(
    State(state): State<AppState>,
    Query(query): Query<OsuQuery>,
) -> Result<Markup, AppError> {
    let current_page = query.page.unwrap_or(1);
    let current_mode = query.mode.unwrap_or(OsuMode::Osu);
    let mut all_players = match current_mode {
        OsuMode::Osu => state.osu_osu_leaderboard,
        OsuMode::Taiko => state.osu_taiko_leaderboard,
        OsuMode::Fruits => state.osu_fruits_leaderboard,
        OsuMode::Mania => state.osu_mania_leaderboard,
    };
    if let Some(pattern) = &query.search {
        all_players = Arc::new(
            all_players
                .iter()
                .filter(|(_, name, _, _)| name.to_lowercase().contains(&pattern.to_lowercase()))
                .cloned()
                .collect(),
        )
    }
    let current_mode_text = match current_mode {
        OsuMode::Osu => "osu",
        OsuMode::Taiko => "taiko",
        OsuMode::Fruits => "fruits",
        OsuMode::Mania => "mania",
    };
    let number_of_pages = all_players.len().div_ceil(50);
    let document = html! {
        (DOCTYPE)
        html lang="en" {
            (osu_head())
            body {
                #title-header {
                    a href="/osu" {
                        #logo-div {
                            img #logo src="/logo.png" alt="Zinfour's logo";
                        }
                    }
                    p { "Zinfour's osu! Leaderboard" }
                }
                #center-div {
                    #osu-blurb {
                        "This is a leaderboard for ";
                        a href="https://osu.ppy.sh/" {"osu!"};
                        " based on leaderboard positions on individual maps. Don't focus on the skill value, it doesn't mean much. Source can be found on ";
                        a href="https://github.com/Zinfour/osu-ranker" {"Github"};
                        "."
                    }
                    #osu-about {
                        "Last updated: 2026-02-01"
                    }
                    #osu-settings {
                        select #sort-by onchange="document.location.href=\"/osu?mode=\" + this.value" {
                            @for (sb, txt) in [
                                (OsuMode::Osu, "Osu"),
                                (OsuMode::Taiko, "Taiko"),
                                (OsuMode::Fruits, "Catch"),
                                (OsuMode::Mania, "Mania"),
                            ] {
                                option value=(sb) selected[query.mode == Some(sb)] { (txt) }
                            }
                        }
                        input.search placeholder="Username" onkeydown="osu_search(this, event)" value=[query.search.as_ref()] {}
                        #osu-nav {
                            @if current_page > 1 {
                                a href=(osu_query_to_url(Some(1), query.search.as_ref(), query.mode)) { (1) }
                            }
                            @if current_page > 4 {
                                a { "..." }
                            }
                            @if current_page > 3 {
                                a href=(osu_query_to_url(Some(current_page-2), query.search.as_ref(), query.mode)) { (current_page-2) }
                            }
                            @if current_page > 2 {
                                a href=(osu_query_to_url(Some(current_page-1), query.search.as_ref(), query.mode)) { (current_page-1) }
                            }
                            a ."active-page" href=(osu_query_to_url(Some(current_page), query.search.as_ref(), query.mode)) { (current_page) }
                            @if number_of_pages.saturating_sub(current_page) > 1 {
                                a href=(osu_query_to_url(Some(current_page+1), query.search.as_ref(), query.mode)) { (current_page+1) }
                            }
                            @if number_of_pages.saturating_sub(current_page) > 2 {
                                a href=(osu_query_to_url(Some(current_page+2), query.search.as_ref(), query.mode)) { (current_page+2) }
                            }
                            @if number_of_pages.saturating_sub(current_page) > 3 {
                                a { "..." }
                            }
                            @if number_of_pages.saturating_sub(current_page) > 0 {
                                a href=(osu_query_to_url(Some(number_of_pages), query.search.as_ref(), query.mode)) { (number_of_pages) }
                            }
                        }
                    }
                    #osu-header {
                        ."leaderboard-rank" {
                            "Rank"
                        }
                        ."leaderboard-username" {
                            "Name"
                        }
                        ."leaderboard-skill" {
                            "Skill"
                        }
                    }
                    #osu-player-list {
                        @for (rank, username, skill, user_id) in all_players.iter().skip(50*(current_page - 1)).take(50) {
                            ."osu-player" {
                                ."leaderboard-rank" {
                                    (rank)
                                }
                                a ."leaderboard-username" href={"https://osu.ppy.sh/users/" (user_id) "/" (current_mode_text)} {
                                    (username)
                                }
                                ."leaderboard-skill" {
                                    (format!("{}", (100.0 * skill).round()))
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    Ok(document)
}

#[axum::debug_handler]
async fn homepage() -> Result<Markup, AppError> {
    let document = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                title { "zinfour.com" }
                link rel="stylesheet" href="/styles.css";
                link rel="preconnect" href="https://fonts.googleapis.com";
                link rel="preconnect" href="https://fonts.gstatic.com" crossorigin;
                link href="https://fonts.googleapis.com/css2?family=Source+Serif+4:ital,opsz,wght@0,8..60,200..900;1,8..60,200..900&display=swap" rel="stylesheet";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                script src="/script.js" {};

                link rel="icon" type="image/x-icon" href="/favicon.ico";

                meta name="twitter:card" content="summary_large_image";
                meta name="twitter:site" content="@zinfour_";
                meta name="twitter:creator" content="@zinfour_";
                meta name="twitter:title" content="zinfour.com";
                meta name="twitter:description" content="a place to read and create interactive fiction";
                meta name="twitter:image" content="https://zinfour.com/adventure/twitter_preview.png";
            }
            body {
                #title-header {
                    a href="/osu" {
                        #logo-div {
                            img #logo src="/logo.png" alt="Zinfour's logo";
                        }
                    }
                    p { "Welcome" }
                }
                #center-div {
                    #project-list {
                        a href="/osu" { "osu! Leaderboard" }
                        a href="/adventure" { "Adventure Database (wip)" }
                        a { "Magic: The Gathering Simulator (wip)" }
                    }
                }
            }
        }
    };
    Ok(document)
}