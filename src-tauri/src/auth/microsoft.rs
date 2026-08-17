//! Comptes Microsoft le Minecraft « premium », officiel.
//!
//! Le flux est celui du **device code** : le launcher n'ouvre pas de fenêtre de
//! connexion, il affiche un code que l'utilisateur saisit dans son navigateur.
//! C'est le seul flux OAuth qui ne demande ni URL de redirection, ni webview
//! détournée en navigateur deux choses qu'une application Tauri sans serveur
//! local paie cher.
//!
//! Quatre étapes derrière l'unique bouton de l'interface :
//!
//! 1. **Microsoft** code d'appareil, puis attente que l'utilisateur valide ;
//! 2. **Xbox Live** le jeton Microsoft s'échange contre un jeton XBL ;
//! 3. **XSTS** le jeton XBL s'échange contre un jeton de service Minecraft ;
//!    c'est ici que remontent « pas de profil Xbox », « compte enfant », etc. ;
//! 4. **Minecraft** le jeton XSTS s'échange contre le jeton que le jeu reçoit
//!    en `--accessToken`, puis le profil donne le pseudo et l'UUID **réels**.
//!
//! Le pseudo et l'UUID viennent donc du serveur, jamais d'un calcul local :
//! c'est la seule identité que le launcher accepte, et rien ici ne peut être
//! fabriqué à partir d'un pseudo choisi.
//!
//! # In English
//!
//! Microsoft accounts for the official, "premium" Minecraft.
//!
//! The flow is the **device code** one: the launcher opens no sign-in window, it
//! displays a code that the user types into their own browser. It is the only
//! OAuth flow that needs neither a redirect URL nor a webview turned into a
//! browser two things a Tauri application without a local server pays dearly
//! for.
//!
//! Four steps behind the interface's single button:
//!
//! 1. **Microsoft** device code, then waiting for the user to approve it;
//! 2. **Xbox Live** the Microsoft token is exchanged for an XBL token;
//! 3. **XSTS** the XBL token is exchanged for a Minecraft service token; this
//!    is where "no Xbox profile", "child account", etc. surface;
//! 4. **Minecraft** the XSTS token is exchanged for the token the game receives
//!    in `--accessToken`, then the profile gives the **real** username and UUID.
//!
//! The username and the UUID therefore come from the server, never from a local
//! computation: it is the only identity the launcher accepts, and nothing here
//! can be fabricated from a chosen username.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::http::Http;

/// Remplace l'identifiant d'application Azure, sans recompiler.
pub const CLIENT_ID_ENV: &str = "GLAUNCHER_MS_CLIENT_ID";

/// L'identifiant d'application Azure AD du launcher.
///
/// Il est **écrit en dur exprès** : l'installateur part chez des joueurs qui ne
/// poseront pas de variable d'environnement, et un identifiant de client public
/// OAuth n'est pas un secret il n'y a rien à protéger, seul le consentement
/// donné par l'utilisateur dans son propre navigateur autorise quoi que ce soit.
///
/// L'application vit dans le « Default Directory » du compte Azure de l'auteur,
/// déclarée en *comptes Microsoft personnels uniquement* (ce que sont tous les
/// comptes Minecraft) avec *Autoriser les flux clients publics* activé sans
/// cette dernière case, Azure réclame un `client_secret` que le launcher n'a
/// pas, et le device code se fait refuser.
///
/// `GLAUNCHER_MS_CLIENT_ID` le remplace, pour tester contre une autre
/// déclaration sans recompiler.
const DEFAULT_CLIENT_ID: &str = "f3b05cee-8075-4d25-bf37-5441fae62747";

/// Le seul périmètre demandé. `offline_access` est ce qui donne le jeton de
/// rafraîchissement sans lui, il faudrait se reconnecter à chaque partie.
const SCOPE: &str = "XboxLive.signin offline_access";

/// `consumers` et non `common` : Minecraft est vendu à des comptes Microsoft
/// personnels, pas à des comptes d'entreprise.
const AUTHORITY: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";

/// Le formulaire qui fait valider une déclaration Azure auprès de Mojang.
/// Sans validation, tout le flux réussit et seule la dernière étape répond 403.
const APP_REVIEW_FORM: &str = "https://aka.ms/mce-reviewappid";

/// Marge avant expiration du jeton Minecraft : renouveler à la seconde près
/// laisserait passer un jeton mort le temps que le jeu démarre.
const EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);

pub fn client_id() -> Result<String> {
    resolve_client_id(
        &std::env::var(CLIENT_ID_ENV).unwrap_or_default(),
        DEFAULT_CLIENT_ID,
    )
}

/// La variable d'environnement gagne sur la valeur compilée. Les deux vides est
/// le cas d'une compilation sans déclaration Azure : mieux vaut une phrase qui
/// nomme la variable à poser qu'un refus de Microsoft dix secondes plus tard.
fn resolve_client_id(from_env: &str, compiled: &str) -> Result<String> {
    let id = if from_env.trim().is_empty() {
        compiled.trim()
    } else {
        from_env.trim()
    };
    if id.is_empty() {
        return Err(Error::other(format!(
            "aucun identifiant d'application Microsoft configuré \
             enregistrez une application Azure (comptes personnels, flux client public) \
             puis renseignez son identifiant dans la variable d'environnement {CLIENT_ID_ENV}"
        )));
    }
    Ok(id.to_string())
}

/// Les URL des quatre services, regroupées pour que les tests puissent les
/// pointer sur un `wiremock` local aucun test ne sort sur Internet.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub device_code: String,
    pub token: String,
    pub xbox_live: String,
    pub xsts: String,
    pub minecraft_login: String,
    pub minecraft_profile: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            device_code: format!("{AUTHORITY}/devicecode"),
            token: format!("{AUTHORITY}/token"),
            xbox_live: "https://user.auth.xboxlive.com/user/authenticate".to_string(),
            xsts: "https://xsts.auth.xboxlive.com/xsts/authorize".to_string(),
            minecraft_login: "https://api.minecraftservices.com/authentication/login_with_xbox"
                .to_string(),
            minecraft_profile: "https://api.minecraftservices.com/minecraft/profile".to_string(),
        }
    }
}

impl Endpoints {
    /// Toutes les routes sur une même base pour les tests.
    #[cfg(test)]
    fn all_on(base: &str) -> Self {
        Self {
            device_code: format!("{base}/devicecode"),
            token: format!("{base}/token"),
            xbox_live: format!("{base}/xbl"),
            xsts: format!("{base}/xsts"),
            minecraft_login: format!("{base}/login_with_xbox"),
            minecraft_profile: format!("{base}/profile"),
        }
    }
}

/// Ce que l'utilisateur doit faire : aller sur `verification_uri` et y taper
/// `user_code`. `device_code`, lui, ne quitte pas le launcher.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Durée de validité du code, en secondes (~15 min).
    pub expires_in: u64,
    /// Intervalle minimal entre deux interrogations, en secondes (~5).
    pub interval: u64,
}

/// Les jetons Microsoft. Le jeton d'accès ne sert qu'à la passe Xbox Live juste
/// après ; seul le jeton de rafraîchissement est conservé.
#[derive(Debug, Clone)]
pub struct MicrosoftTokens {
    pub access_token: String,
    pub refresh_token: String,
}

/// L'issue d'une interrogation du point de terminaison de jeton.
#[derive(Debug)]
pub enum Poll {
    /// L'utilisateur n'a pas encore validé.
    Pending,
    /// Idem, mais Microsoft demande d'espacer les appels.
    SlowDown,
    Ready(Box<MicrosoftTokens>),
}

/// Une session Minecraft complète, telle que le jeu la veut.
#[derive(Debug, Clone)]
pub struct MinecraftSession {
    pub uuid: Uuid,
    pub username: String,
    /// Ce que `--accessToken` porte. Valable 24 h.
    pub access_token: String,
    pub expires_at_ms: u64,
    /// L'identifiant Xbox, passé au jeu en `${auth_xuid}`.
    pub xuid: Option<String>,
}

/// Millisecondes depuis l'epoch. Une horloge antérieure à 1970 est absurde ; on
/// la traite comme « maintenant » plutôt que de propager une erreur.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// --- étape 1 : Microsoft -----------------------------------------------------

pub async fn request_device_code(
    http: &Http,
    endpoints: &Endpoints,
    client_id: &str,
) -> Result<DeviceCode> {
    let url = &endpoints.device_code;
    let response = http
        .client()
        .post(url)
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|source| Error::Http {
            url: url.clone(),
            source,
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error: TokenError = response.json().await.unwrap_or_default();
        return Err(Error::other(format!(
            "Microsoft a refusé la demande de code ({status}) : {}",
            error.describe()
        )));
    }
    response.json().await.map_err(|source| Error::Http {
        url: url.clone(),
        source,
    })
}

/// Une interrogation, une réponse. La boucle d'attente est chez l'appelant, qui
/// est le seul à savoir quand l'utilisateur a annulé.
pub async fn poll_token(
    http: &Http,
    endpoints: &Endpoints,
    client_id: &str,
    device_code: &str,
) -> Result<Poll> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", device_code),
    ];
    match post_token(http, &endpoints.token, &form).await? {
        Ok(tokens) => Ok(Poll::Ready(Box::new(tokens))),
        // Les deux seules réponses qui veulent dire « continue d'attendre ».
        Err(error) if error.error == "authorization_pending" => Ok(Poll::Pending),
        Err(error) if error.error == "slow_down" => Ok(Poll::SlowDown),
        Err(error) => Err(Error::other(match error.error.as_str() {
            "expired_token" | "code_expired" => {
                "le code de connexion a expiré, recommencez".to_string()
            }
            "authorization_declined" => "connexion refusée dans le navigateur".to_string(),
            "bad_verification_code" => "code de connexion invalide".to_string(),
            _ => error.describe(),
        })),
    }
}

pub async fn refresh_tokens(
    http: &Http,
    endpoints: &Endpoints,
    client_id: &str,
    refresh_token: &str,
) -> Result<MicrosoftTokens> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", SCOPE),
    ];
    post_token(http, &endpoints.token, &form)
        .await?
        .map_err(|error| {
            // `invalid_grant` = mot de passe changé, session révoquée, ou six
            // mois sans jouer. Rien à retenter : il faut se reconnecter.
            if error.error == "invalid_grant" {
                Error::other("session Microsoft expirée, reconnectez le compte")
            } else {
                Error::other(error.describe())
            }
        })
}

// --- étapes 2 à 4 : Xbox Live, XSTS, Minecraft -------------------------------

/// Le reste de la chaîne, depuis un jeton d'accès Microsoft frais.
pub async fn minecraft_session(
    http: &Http,
    endpoints: &Endpoints,
    microsoft_access_token: &str,
) -> Result<MinecraftSession> {
    let xbl = xbox_live_authenticate(http, endpoints, microsoft_access_token).await?;
    let xsts = xsts_authorize(http, endpoints, &xbl.token).await?;
    let user_hash = xsts
        .user_hash()
        .or_else(|| xbl.user_hash())
        .ok_or_else(|| Error::other("réponse Xbox Live sans identifiant d'utilisateur"))?;

    let login = minecraft_login(http, endpoints, &user_hash, &xsts.token).await?;
    let profile = minecraft_profile(http, endpoints, &login.access_token).await?;

    Ok(MinecraftSession {
        uuid: profile.uuid()?,
        username: profile.name,
        access_token: login.access_token,
        // `expires_in` est en secondes ; 24 h en pratique.
        expires_at_ms: now_ms() + login.expires_in.saturating_mul(1000),
        xuid: xsts.xuid(),
    })
}

/// Le jeton Minecraft est-il encore bon pour lancer une partie ?
pub fn is_fresh(expires_at_ms: u64) -> bool {
    now_ms() + EXPIRY_MARGIN.as_millis() as u64 <= expires_at_ms
}

async fn xbox_live_authenticate(
    http: &Http,
    endpoints: &Endpoints,
    microsoft_access_token: &str,
) -> Result<XboxToken> {
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            // Le `d=` est obligatoire pour un jeton obtenu hors du SDK Xbox.
            "RpsTicket": format!("d={microsoft_access_token}"),
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    });
    let response = post_json(http, &endpoints.xbox_live, &body).await?;
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "Xbox Live a refusé la connexion ({})",
            response.status()
        )));
    }
    response.json().await.map_err(|source| Error::Http {
        url: endpoints.xbox_live.clone(),
        source,
    })
}

async fn xsts_authorize(http: &Http, endpoints: &Endpoints, xbl_token: &str) -> Result<XboxToken> {
    let body = serde_json::json!({
        "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT",
    });
    let response = post_json(http, &endpoints.xsts, &body).await?;

    // C'est ici que se voient les comptes sans profil Xbox et les comptes
    // enfants : un 401 avec un code `XErr` qui dit précisément quoi faire.
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let error: XstsError = response.json().await.unwrap_or_default();
        return Err(Error::other(error.describe()));
    }
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "XSTS a refusé la connexion ({})",
            response.status()
        )));
    }
    response.json().await.map_err(|source| Error::Http {
        url: endpoints.xsts.clone(),
        source,
    })
}

async fn minecraft_login(
    http: &Http,
    endpoints: &Endpoints,
    user_hash: &str,
    xsts_token: &str,
) -> Result<MinecraftLogin> {
    let body = serde_json::json!({
        "identityToken": format!("XBL3.0 x={user_hash};{xsts_token}"),
    });
    let response = post_json(http, &endpoints.minecraft_login, &body).await?;

    // Le compte et ses jetons Xbox sont bons c'est *l'application* que
    // Mojang refuse. Depuis 2023, une déclaration Azure neuve n'a pas accès à
    // l'API Minecraft tant qu'elle n'a pas été validée par ce formulaire, et le
    // seul symptôme est ce 403 sec. Sans cette phrase, on chercherait le
    // problème du côté du compte pendant des heures.
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::other(format!(
            "l'application n'est pas autorisée par Mojang à utiliser l'API Minecraft \
             (403). Une déclaration Azure neuve doit être validée : formulaire sur \
             {APP_REVIEW_FORM}, réponse sous quelques jours. En attendant, les comptes \
             hors-ligne fonctionnent."
        )));
    }
    if !response.status().is_success() {
        let status = response.status();
        // Le corps porte parfois `{ "error", "errorMessage" }` ; le jeter
        // laisserait l'utilisateur avec un nombre à trois chiffres.
        let detail = response.text().await.unwrap_or_default();
        return Err(Error::other(match detail.trim() {
            "" => format!("Minecraft a refusé la connexion ({status})"),
            detail => format!("Minecraft a refusé la connexion ({status}) : {detail}"),
        }));
    }
    response.json().await.map_err(|source| Error::Http {
        url: endpoints.minecraft_login.clone(),
        source,
    })
}

async fn minecraft_profile(
    http: &Http,
    endpoints: &Endpoints,
    access_token: &str,
) -> Result<Profile> {
    let url = &endpoints.minecraft_profile;
    let response = http
        .client()
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|source| Error::Http {
            url: url.clone(),
            source,
        })?;

    // Le compte existe, il est authentifié, il n'a simplement pas le jeu le
    // seul cas où l'utilisateur doit aller acheter quelque chose.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(Error::other(
            "ce compte Microsoft ne possède pas Minecraft: Java Edition",
        ));
    }
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "profil Minecraft irrécupérable ({})",
            response.status()
        )));
    }
    response.json().await.map_err(|source| Error::Http {
        url: url.clone(),
        source,
    })
}

// --- plomberie ---------------------------------------------------------------

/// `Ok(Err(TokenError))` = le serveur a répondu proprement qu'il refusait ;
/// `Err(_)` = la requête elle-même a échoué. Les deux ne se traitent pas
/// pareil : la première est attendue à chaque tour de la boucle d'attente.
#[allow(clippy::result_large_err)]
async fn post_token(
    http: &Http,
    url: &str,
    form: &[(&str, &str)],
) -> Result<std::result::Result<MicrosoftTokens, TokenError>> {
    let response = http
        .client()
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|source| Error::Http {
            url: url.to_string(),
            source,
        })?;

    if response.status().is_success() {
        let body: TokenResponse = response.json().await.map_err(|source| Error::Http {
            url: url.to_string(),
            source,
        })?;
        return Ok(Ok(MicrosoftTokens {
            access_token: body.access_token,
            refresh_token: body.refresh_token,
        }));
    }

    let status = response.status();
    let mut error: TokenError = response.json().await.unwrap_or_default();
    if error.error.is_empty() {
        error.error = format!("http_{status}");
    }
    Ok(Err(error))
}

async fn post_json(http: &Http, url: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
    http.client()
        .post(url)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|source| Error::Http {
            url: url.to_string(),
            source,
        })
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Default, Deserialize)]
struct TokenError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

impl TokenError {
    fn describe(&self) -> String {
        // La description de Microsoft est en anglais et bavarde (elle contient
        // un identifiant de corrélation), mais elle est la seule à dire ce qui
        // ne va pas quand le code d'erreur n'est pas dans notre liste.
        self.error_description
            .clone()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| self.error.clone())
    }
}

/// La réponse de Xbox Live et de XSTS ont la même forme.
#[derive(Debug, Deserialize)]
struct XboxToken {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims", default)]
    display_claims: Option<DisplayClaims>,
}

#[derive(Debug, Deserialize)]
struct DisplayClaims {
    #[serde(default)]
    xui: Vec<Xui>,
}

#[derive(Debug, Deserialize)]
struct Xui {
    #[serde(default)]
    uhs: Option<String>,
    #[serde(default)]
    xid: Option<String>,
}

impl XboxToken {
    fn first_claim(&self) -> Option<&Xui> {
        self.display_claims.as_ref()?.xui.first()
    }

    fn user_hash(&self) -> Option<String> {
        self.first_claim()?.uhs.clone()
    }

    fn xuid(&self) -> Option<String> {
        self.first_claim()?.xid.clone()
    }
}

#[derive(Debug, Default, Deserialize)]
struct XstsError {
    #[serde(rename = "XErr", default)]
    xerr: u64,
    #[serde(rename = "Message", default)]
    message: Option<String>,
}

impl XstsError {
    /// Les codes que l'utilisateur peut corriger lui-même méritent une phrase
    /// qui dit quoi faire ; le reste retombe sur le message brut.
    fn describe(&self) -> String {
        match self.xerr {
            2148916227 => "ce compte Xbox Live a été suspendu".to_string(),
            2148916233 => "ce compte Microsoft n'a pas de profil Xbox créez-en un sur \
                           xbox.com, puis réessayez"
                .to_string(),
            2148916235 => "Xbox Live n'est pas disponible dans le pays de ce compte".to_string(),
            2148916236 | 2148916237 => {
                "ce compte doit passer la vérification d'adulte (Corée du Sud)".to_string()
            }
            2148916238 => "ce compte est un compte enfant : rattachez-le à une famille \
                           Microsoft, puis réessayez"
                .to_string(),
            other => self
                .message
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| format!("Xbox Live a refusé la connexion (XErr {other})")),
        }
    }
}

#[derive(Debug, Deserialize)]
struct MinecraftLogin {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct Profile {
    /// UUID sans tirets, tel que Mojang l'écrit.
    id: String,
    name: String,
}

impl Profile {
    fn uuid(&self) -> Result<Uuid> {
        Uuid::parse_str(&self.id)
            .map_err(|_| Error::other(format!("UUID de profil illisible: {}", self.id)))
    }
}

/// Ce qui est persisté pour rejouer une session sans redemander le code.
///
/// Le jeton de rafraîchissement vit **en clair** dans `accounts.json`, comme le
/// jeton du compte gLauncher dans `sync.json`. Le Credential Manager de Windows
/// aurait été plus propre, mais il plafonne un secret à 2560 octets et un jeton
/// de rafraîchissement Microsoft frôle cette limite une fois encodé en UTF-16 :
/// un compte sur deux aurait refusé de s'enregistrer. Le périmètre du jeton se
/// limite de toute façon à `XboxLive.signin`, et le fichier reste dans le profil
/// Windows de l'utilisateur.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrosoftCredentials {
    pub refresh_token: String,
    /// Le jeton que le jeu reçoit court-vécu, renouvelable sans l'utilisateur.
    pub access_token: String,
    /// Millisecondes epoch.
    pub expires_at_ms: u64,
    #[serde(default)]
    pub xuid: Option<String>,
}

impl MicrosoftCredentials {
    pub fn is_fresh(&self) -> bool {
        is_fresh(self.expires_at_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn the_compiled_client_id_is_a_real_application_id() {
        // Un GUID mal recopié ne se verrait qu'au premier essai de connexion,
        // avec un refus de Microsoft qui ne dit pas d'où vient le problème.
        let id = client_id().expect("un identifiant est compilé");
        assert!(Uuid::parse_str(&id).is_ok(), "identifiant illisible: {id}");
    }

    #[test]
    fn the_environment_wins_and_two_empties_name_the_variable_to_set() {
        assert_eq!(
            resolve_client_id("  de-la-variable  ", "compile").unwrap(),
            "de-la-variable"
        );
        assert_eq!(resolve_client_id("   ", "compile").unwrap(), "compile");

        let error = resolve_client_id("", "").unwrap_err().to_string();
        assert!(error.contains(CLIENT_ID_ENV), "{error}");
    }

    #[tokio::test]
    async fn the_device_code_request_asks_for_the_offline_scope() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/devicecode"))
            .and(body_string_contains("offline_access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "DEV",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://microsoft.com/link",
                "expires_in": 900,
                "interval": 5,
                "message": "…"
            })))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let code = request_device_code(&http, &Endpoints::all_on(&server.uri()), "cli")
            .await
            .unwrap();
        assert_eq!(code.user_code, "ABCD-EFGH");
        assert_eq!(code.interval, 5);
        assert_eq!(code.device_code, "DEV");
    }

    #[tokio::test]
    async fn waiting_for_the_user_is_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("device_code=EN-ATTENTE"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
                "error_description": "The user has not yet completed authentication."
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("device_code=TROP-VITE"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "slow_down" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("device_code=PERIME"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "expired_token" })),
            )
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let endpoints = Endpoints::all_on(&server.uri());

        assert!(matches!(
            poll_token(&http, &endpoints, "cli", "EN-ATTENTE")
                .await
                .unwrap(),
            Poll::Pending
        ));
        assert!(matches!(
            poll_token(&http, &endpoints, "cli", "TROP-VITE")
                .await
                .unwrap(),
            Poll::SlowDown
        ));
        // Celle-là, en revanche, est bien une fin de partie.
        let error = poll_token(&http, &endpoints, "cli", "PERIME")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("expiré"), "{error}");
    }

    #[tokio::test]
    async fn a_validated_code_yields_both_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_type": "Bearer",
                "expires_in": 3600,
                "access_token": "MS-ACCESS",
                "refresh_token": "MS-REFRESH"
            })))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let outcome = poll_token(&http, &Endpoints::all_on(&server.uri()), "cli", "DEV")
            .await
            .unwrap();
        let Poll::Ready(tokens) = outcome else {
            panic!("jetons attendus, obtenu {outcome:?}");
        };
        assert_eq!(tokens.access_token, "MS-ACCESS");
        assert_eq!(tokens.refresh_token, "MS-REFRESH");
    }

    #[tokio::test]
    async fn a_revoked_refresh_token_says_to_sign_in_again() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "AADSTS70000: The provided grant has expired."
            })))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let error = refresh_tokens(&http, &Endpoints::all_on(&server.uri()), "cli", "vieux")
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "session Microsoft expirée, reconnectez le compte"
        );
    }

    /// Monte la chaîne complète Xbox Live → XSTS → Minecraft → profil.
    async fn serve_happy_chain() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xbl"))
            // La preuve que le ticket porte bien son préfixe `d=`.
            .and(body_string_contains("d=MS-ACCESS"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "IssueInstant": "2026-01-01T00:00:00Z",
                "Token": "XBL-TOKEN",
                "DisplayClaims": { "xui": [{ "uhs": "hachage-utilisateur" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/xsts"))
            .and(body_string_contains("XBL-TOKEN"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "XSTS-TOKEN",
                "DisplayClaims": { "xui": [{ "uhs": "hachage-utilisateur", "xid": "2535" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login_with_xbox"))
            .and(body_string_contains(
                "XBL3.0 x=hachage-utilisateur;XSTS-TOKEN",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "username": "peu-importe",
                "access_token": "JETON-MINECRAFT",
                "token_type": "Bearer",
                "expires_in": 86400
            })))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_full_chain_returns_the_server_side_identity() {
        let server = serve_happy_chain().await;
        Mock::given(method("GET"))
            .and(path("/profile"))
            .and(header("authorization", "Bearer JETON-MINECRAFT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "069a79f444e94726a5befca90e38aaf5",
                "name": "Notch"
            })))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let session = minecraft_session(&http, &Endpoints::all_on(&server.uri()), "MS-ACCESS")
            .await
            .unwrap();

        assert_eq!(session.username, "Notch");
        // L'UUID vient du serveur, jamais du pseudo.
        assert_eq!(
            session.uuid.to_string(),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
        assert_eq!(session.access_token, "JETON-MINECRAFT");
        assert_eq!(session.xuid.as_deref(), Some("2535"));
        assert!(is_fresh(session.expires_at_ms));
    }

    #[tokio::test]
    async fn an_account_without_the_game_is_told_so_plainly() {
        let server = serve_happy_chain().await;
        Mock::given(method("GET"))
            .and(path("/profile"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let error = minecraft_session(&http, &Endpoints::all_on(&server.uri()), "MS-ACCESS")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("ne possède pas Minecraft"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_unapproved_azure_app_blames_the_app_not_the_account() {
        // Tout passe jusqu'au bout, puis Mojang répond 403 : c'est la
        // déclaration Azure qui n'est pas validée, pas le compte du joueur.
        // Confondre les deux envoie chercher le problème au mauvais endroit.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xbl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "XBL-TOKEN",
                "DisplayClaims": { "xui": [{ "uhs": "hachage-utilisateur" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/xsts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "XSTS-TOKEN",
                "DisplayClaims": { "xui": [{ "uhs": "hachage-utilisateur" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/login_with_xbox"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let error = minecraft_session(&http, &Endpoints::all_on(&server.uri()), "MS-ACCESS")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("pas autorisée par Mojang"), "{error}");
        // Et le message porte l'adresse du formulaire, seule action possible.
        assert!(error.contains(APP_REVIEW_FORM), "{error}");
    }

    #[tokio::test]
    async fn an_account_without_an_xbox_profile_gets_actionable_advice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xbl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Token": "XBL-TOKEN",
                "DisplayClaims": { "xui": [{ "uhs": "hachage-utilisateur" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/xsts"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "Identity": "0",
                "XErr": 2148916233u64,
                "Message": "",
                "Redirect": "https://start.ui.xboxlive.com/CreateAccount"
            })))
            .mount(&server)
            .await;

        let http = Http::new().unwrap();
        let error = minecraft_session(&http, &Endpoints::all_on(&server.uri()), "MS-ACCESS")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("xbox.com"), "{error}");

        // Et le compte enfant, l'autre cas fréquent.
        assert!(XstsError {
            xerr: 2148916238,
            message: None
        }
        .describe()
        .contains("compte enfant"));
    }

    #[test]
    fn freshness_leaves_a_margin_before_expiry() {
        let now = now_ms();
        assert!(!is_fresh(now));
        // Juste avant l'expiration : périmé pour nous, le jeu ne démarrerait
        // pas à temps.
        assert!(!is_fresh(now + 60 * 1000));
        assert!(is_fresh(now + 60 * 60 * 1000));
    }
}
