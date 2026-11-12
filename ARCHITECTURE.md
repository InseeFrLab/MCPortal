# Architecture de MCPortal

Ce document décrit l'organisation du code et le fonctionnement interne de la passerelle.
Pour l'utilisation, voir le [README](README.md). Les choix de conception et leur
justification sont regroupés à la fin, dans [Décisions de conception](#décisions-de-conception).

Principe directeur : **la simplicité avant tout**. Un seul crate, un état partagé immuable,
pas de verrou sur le chemin des requêtes, et le protocole MCP délégué au SDK officiel
[`rmcp`](https://crates.io/crates/rmcp).

## Vue d'ensemble

```text
                       ┌──────────────────────── MCPortal ────────────────────────┐
                       │                                                          │
 client MCP ──HTTP──▶  │  server.rs ──▶ gateway.rs (ServerHandler rmcp)            │
 (aval)       :8000    │  /mcp /health    ├─ search_tools ──▶ search.rs (BM25)      │
                       │  /ready          ├─ call_tool ─────▶ limits.rs ──┐         │
                       │                  ├─ send_feedback ─▶ logs        │         │
                       │                  └─ resources ─────▶ catalog.rs  │         │
                       │                                      guide.rs    ▼         │
                       │  state.rs  AppState ─────────────────────▶ upstream.rs ───┼──HTTP──▶ amonts
                       │   ├─ config (fixe)                         (client rmcp)  │
                       │   ├─ upstreams (fixes)                                    │
                       │   └─ catalog: ArcSwap<ToolCatalog>  ◀── boucle de        │
                       │                                         rafraîchissement  │
 Prometheus ──HTTP──▶  │  /metrics :9090  ◀── telemetry.rs                         │
                       └──────────────────────────────────────────────────────────┘
```

## Arborescence

```text
.
├── Cargo.toml
├── registry.yaml            registre de démonstration
├── guide-ssp.md             guide propre au déploiement (servi après le guide générique)
├── src/
│   ├── main.rs              binaire : lit la config, installe logs et métriques, lance server::serve
│   ├── lib.rs               racine de la bibliothèque, ordre de lecture conseillé
│   ├── config.rs            types du registre YAML + validation (pur, sans réseau)
│   ├── upstream.rs          un serveur amont : connexion, appels, santé, cache d'outils
│   ├── state.rs             AppState, catalogue d'outils, boucle de rafraîchissement
│   ├── search.rs            index BM25 (trait ToolIndex)
│   ├── gateway.rs           serveur MCP exposé : outils et ressources
│   ├── catalog.rs           contenu JSON des ressources catalog://…
│   ├── guide.rs             assemblage du guide + garde-fous contre sa dérive
│   ├── guide.md             guide générique, compilé dans le binaire
│   ├── limits.rs            sémaphores de concurrence
│   ├── server.rs            routeurs axum, /health, /ready, /metrics, arrêt propre
│   └── telemetry.rs         logs tracing (JSON) + export Prometheus, noms des métriques
├── tests/
│   ├── common/mod.rs        amont MCP simulé, démarrage de passerelle, client de test
│   ├── gateway.rs           tests de bout en bout (client → passerelle → amont simulé)
│   └── guide_drift.rs       cohérence du guide de déploiement avec le registre réel
├── benches/overhead.rs      mesure de l'overhead (criterion)
├── deploy/kubernetes.yaml   exemple de déploiement
└── Dockerfile
```

Toute la logique vit dans la **bibliothèque** (`src/lib.rs`) ; `main.rs` ne fait
qu'assembler. C'est ce qui permet aux tests et au benchmark de démarrer une vraie
passerelle en mémoire.

## Les modules

### `config` : le registre

Types `serde` du fichier YAML (`Config`, `GatewayConfig`, `UpstreamConfig`…) avec
`deny_unknown_fields` et une valeur par défaut pour chaque clé. `Config::load` résout
`guide_path` par rapport au fichier de registre et vérifie qu'il existe. `Config::from_yaml`
permet de tester sans toucher au disque.

### `upstream` : parler à un serveur amont

Un `Upstream` par serveur du registre. Il contient :

- **une connexion rmcp partagée** par tous les utilisateurs. En Streamable HTTP, chaque appel
  est un POST indépendant : un seul client suffit pour des appels concurrents. La connexion
  est ouverte au premier besoin et refaite si le transport se ferme ;
- **un sémaphore** de `max_concurrency` places ;
- **l'état de santé** (échecs consécutifs, dernière erreur, horodatages) et des compteurs ;
- **le cache de la liste d'outils**, daté pour respecter `cache_ttl`.

La version du protocole est négociée en mode `Auto` : `server/discover` (2026-07-28) d'abord,
puis repli sur `initialize` pour les amonts plus anciens.

Toutes les erreurs amont sont traduites en `UpstreamError` (`Unavailable`, `Timeout`,
`Protocol`). Une erreur de transport invalide la connexion, qui sera refaite à l'appel
suivant.

### `state` : l'état partagé

`AppState` est créé une fois et partagé par `Arc`. Il sépare :

| Donnée | Durée de vie | Accès |
| --- | --- | --- |
| `config`, `upstreams`, `limits` | vie du pod | lecture directe |
| `catalog: ArcSwap<ToolCatalog>` | remplacé quand un amont change ses outils | `load()` sans verrou |
| `ready: AtomicBool` | change au démarrage et à l'arrêt | atomique |

Le `ToolCatalog` est **immuable** : la table `nom exposé → ToolEntry` (en `BTreeMap`, pour un
`tools/list` à l'ordre déterministe) et l'index BM25. Chaque `ToolEntry` porte la définition
exposée (renommée en `{serveur}_{outil}`), le nom amont et le validateur JSON Schema
compilé.

Pour changer le catalogue, `rebuild_catalog` en construit un nouveau à côté, puis le publie
d'un coup avec `store()`. Les requêtes en cours gardent l'ancienne version jusqu'à leur fin :
aucune ne voit un catalogue à moitié construit. Un verrou sérialise les reconstructions,
pour qu'une version périmée ne soit jamais publiée après une plus récente.

### `search` : la recherche BM25

`Bm25ToolIndex` s'appuie sur le crate `bm25` avec un tokenizer français (accents retirés, mots
vides, racinisation). La qualité vient surtout du document indexé pour chaque outil :

- le nom exposé, découpé sur `_` et répété (pour lui donner plus de poids) ;
- le nom, le domaine, les tags et la description du serveur ;
- la description de l'outil ;
- les noms et descriptions de ses paramètres.

Le module ne dépend que de `rmcp::model::Tool`, derrière le trait `ToolIndex`, pour pouvoir
être extrait (crate séparé ou contribution à rmcp).

### `gateway` : le serveur MCP exposé

`Gateway` implémente `ServerHandler` **à la main** plutôt qu'avec les macros `#[tool]`, parce
que la liste des outils dépend des amonts. Les arguments des outils de la passerelle sont des
structures `serde` + `schemars` (`SearchToolsArgs`, `CallToolArgs`, `SendFeedbackArgs`) ; leurs
commentaires `///` deviennent les descriptions du JSON Schema vu par le LLM.

`CallToolArgs` reprend exactement la forme du protocole (`name` + `arguments` en objet JSON) :
les arguments de l'outil cible ne sont connus qu'à l'exécution, on ne les type pas.

### `catalog` et `guide` : les ressources

`catalog.rs` produit le JSON des ressources à partir de l'état courant, à chaque lecture : il
n'existe aucune seconde source de vérité. `guide.rs` assemble `catalog://guide` : le guide
générique (`src/guide.md`, compilé dans le binaire), puis le guide de déploiement relu sur
le disque. Il sait aussi extraire les serveurs et outils cités par un guide, ce qu'utilisent
les tests de dérive.

### `limits` : la concurrence

Deux sémaphores acquis dans l'ordre **global puis serveur**, avec un seul délai pour les deux.
Les places sont rendues automatiquement à la destruction du `CallPermit`, y compris en cas
d'erreur ou d'annulation. Au-delà du délai, l'appel est refusé avec un message lisible.

### `server` et `main` : HTTP et cycle de vie

`router()` monte le service MCP de rmcp sur `gateway.http.path`, avec :

- `legacy_session_mode(false)` : toutes les requêtes sont traitées sans session ;
- `json_response(true)` : réponse JSON simple quand l'outil n'émet pas de notification
  (rmcp repasse en SSE sinon) ;
- un contrôle de l'en-tête `Host` désactivé par défaut (adapté derrière un ingress).

`serve()` lance les tâches de fond (premier chargement, rafraîchissement, port métriques) et
gère l'arrêt.

### `telemetry` : logs et métriques

Logs `tracing` sur la sortie standard, en JSON aplati. Métriques via la façade `metrics`,
agrégées par l'enregistreur Prometheus. Tous les noms de métriques sont regroupés dans
`telemetry::metric`.

## Cycle de vie d'un appel `call_tool`

```text
client ──tools/call {name: "call_tool", arguments: {name, arguments}}──▶ rmcp ──▶ Gateway::call_tool
  1. parse CallToolArgs                     échec → erreur JSON-RPC invalid_params
  2. catalog.load()                         lecture sans verrou de la version courante
  3. résolution du nom exposé               inconnu → isError + noms proches (BM25)
  4. validation JSON Schema                 invalide → isError + liste des erreurs
  5. limits.acquire (global puis serveur)   délai dépassé → isError « saturé »
  6. upstream.call_tool                     timeout, annulation, relais de progression
  7. résultat amont renvoyé tel quel        erreur amont → isError lisible
  8. métriques + log « appel d'outil »
```

Le résultat amont (`content`, `structuredContent`, `isError`, `_meta`) n'est jamais
réemballé : images, ressources embarquées et contenu structuré traversent intacts.

### Deux niveaux d'erreur

| Situation | Réponse |
| --- | --- |
| Requête mal formée (arguments de nos outils non désérialisables) | erreur JSON-RPC |
| Tout ce que le LLM peut corriger ou contourner : outil inconnu, arguments invalides, saturation, amont injoignable, timeout, erreur de l'outil | `CallToolResult` avec `isError: true` et un message actionnable |

C'est la recommandation de la spec MCP : les clients affichent souvent les erreurs JSON-RPC de
façon opaque, alors qu'un `isError` est lu par le LLM.

### Relais vers l'amont : points délicats

- **Annulation** : si le client aval annule (`context.ct`) ou si le timeout expire, le
  future de l'appel est abandonné et `notifications/cancelled` part vers l'amont.
- **Progression** : le client rmcp impose son propre `progressToken` à chaque requête (un
  compteur par connexion). On envoie donc la requête, on lit le jeton choisi
  (`RequestHandle::progress_token`), puis on enregistre la route « jeton amont → client aval »
  dans le `ProgressRouter` **de cette connexion**. Une notification arrivée avant
  l'enregistrement est conservée et relayée ensuite. Le client aval reçoit son propre jeton.
- **Échanges multi-tours (MRTR)** : un `InputRequiredResult` amont est renvoyé tel quel au
  client aval. Quand celui-ci rappelle `call_tool` avec `inputResponses` / `requestState`, ces
  champs sont retransmis à l'amont. La passerelle ne garde aucun état entre les tours.

## Rafraîchissement et santé des amonts

```text
démarrage ──▶ initial_load : tous les amonts en parallèle ──▶ ready = true
                                                                │
              une tâche par amont, toutes les probe_interval_seconds
                                                                ▼
              cache plus vieux que cache_ttl ? ──oui──▶ tools/list complet
                     │                                   └─ liste changée ? ──▶ rebuild_catalog
                     non
                     └──▶ sonde légère (première page de tools/list)
              puis : record_probe → santé, métrique mcportal_upstream_up
```

Un amont devient `unavailable` après `failure_threshold` échecs consécutifs. Ses derniers
outils connus restent dans le catalogue ; un appel est tout de même tenté, ce qui permet de
retrouver l'amont sans attendre la sonde suivante.

## Concurrence et threads

Runtime `tokio` multi-thread ; chaque requête HTTP est une tâche. Règles suivies :

- aucun appel bloquant sur le chemin des requêtes (le guide est lu avec `tokio::fs`) ;
- `std::sync::Mutex` seulement pour des sections critiques très courtes, sans `await` ;
- `tokio::sync::Mutex` pour la connexion amont, tenue pendant la connexion afin que deux
  requêtes simultanées n'ouvrent qu'une seule connexion ;
- `ArcSwap` pour la donnée lue partout et écrite rarement (le catalogue).

## Démarrage et arrêt

```text
démarrage : Config::load ─▶ logs ─▶ Prometheus ─▶ écoute des ports ─▶ /health = 200
            ─▶ initial_load (en tâche de fond) ─▶ /ready = 200

SIGTERM   : /ready = 503 ─▶ axum n'accepte plus de connexions et attend les requêtes en cours
            ─▶ annulation des tâches de fond ─▶ sortie
```

`/health` ne dépend pas des amonts : un amont en panne ne doit pas faire redémarrer la
passerelle. `/ready` attend le premier chargement des outils, pour qu'un nouveau pod ne reçoive
pas de trafic avec un catalogue vide pendant un rolling update.

## Observabilité

| Métrique | Type | Labels |
| --- | --- | --- |
| `mcportal_tool_calls_total` | counter | server, tool, status |
| `mcportal_tool_call_duration_seconds` | histogram | server |
| `mcportal_gateway_overhead_seconds` | histogram | — |
| `mcportal_upstream_up` | gauge | server |
| `mcportal_inflight` | gauge | server |
| `mcportal_ratelimit_rejections_total` | counter | scope (global, server) |
| `mcportal_search_queries_total` | counter | — |
| `mcportal_catalog_tools` | gauge | — |
| `mcportal_feedback_total` | counter | server |

Valeurs de `status` : `ok`, `tool_error`, `upstream_error`, `timeout`, `rejected`, `invalid`,
`cancelled`. Aucune métrique ne porte d'identifiant d'utilisateur, pour éviter l'explosion de
cardinalité.

Chaque appel relayé produit un log `"message": "appel d'outil"` avec `server`, `tool`,
`status`, `duration_ms` et `upstream_ms`. Les retours de `send_feedback` portent
`"label": "FEEDBACK"`.

## Stratégie de test

| Niveau | Où | Ce qui est vérifié |
| --- | --- | --- |
| Unitaire | `#[cfg(test)]` dans chaque module | config, BM25, sémaphores, extraction des citations du guide |
| Intégration | `tests/gateway.rs` | de bout en bout avec de vrais serveurs MCP simulés : recherche, appels, erreurs, timeout, saturation, catalogue, guide, progression, clients anciens, arrêt propre |
| Dérive du guide | `tests/guide_drift.rs` (ignoré par défaut) | serveurs cités activés dans le registre ; outils cités présents chez les vrais amonts |
| Performance | `benches/overhead.rs` | appel direct vs via la passerelle |

## Décisions de conception

Cette section garde la trace du **pourquoi** des choix structurants, pour qu'on puisse les
remettre en cause en connaissance de cause.

### Protocole MCP 2026-07-28 et mode stateless

La révision 2026-07-28 de la spec MCP supprime les sessions (`Mcp-Session-Id`) et la poignée
de main `initialize` : chaque requête porte sa version de protocole, les capacités et
l'identité du client dans `_meta`.

- **Côté aval**, la passerelle sert toutes les requêtes sans session, y compris pour les
  clients plus anciens (`legacy_session_mode(false)`). Conséquence : n'importe quel pod peut
  servir n'importe quelle requête. Plusieurs réplicas fonctionnent sans sessions sticky, et un
  redémarrage de pod ne coupe aucun client.
- **Côté amont**, un client rmcp partagé par serveur. Les amonts encore en version 2025-xx
  gardent leurs sessions, gérées par rmcp (réouverture automatique d'une session expirée).
- `tools/list` renvoie `ttlMs` et `cacheScope: public` : la liste est la même pour tous les
  clients.

### Pas de rechargement à chaud de la configuration

L'objectif était d'éviter les coupures lors d'un changement de config sur Kubernetes. Avec un
protocole sans session, un **rolling update** suffit et ne coupe personne :

1. le ConfigMap change ; une annotation `checksum/config` (Helm) ou l'outil Reloader
   déclenche le rolling update ;
2. le nouveau pod démarre et ne se déclare prêt qu'après le premier chargement des outils ;
3. l'ancien pod reçoit SIGTERM, se déclare non prêt et termine ses appels en cours.

Ce qui rend cela possible, c'est l'arrêt propre décrit plus haut, avec côté Kubernetes un
`preStop` de quelques secondes et un `terminationGracePeriodSeconds` supérieur au plus grand
`timeout` amont.

Le coût d'un rechargement à chaud aurait été modéré (environ 200 lignes), mais avec des
pièges : sémaphores redimensionnés pendant que des places sont prises, paramètres non
rechargeables (port d'écoute), détection fiable des mises à jour de ConfigMap (remplacement
atomique d'un lien symbolique). Si le besoin revient, l'`ArcSwap` du catalogue fournit déjà le
mécanisme d'échange : il resterait à surveiller le fichier et à comparer les amonts.

### Concurrence et limites : globales en V1

Deux limites, globale et par serveur, protègent les amonts sans avoir besoin de savoir qui
appelle. Il n'y a ni authentification ni limite par utilisateur en V1.

Points à retenir pour la suite :

- **Le client MCP n'identifie pas l'utilisateur** : `clientInfo` décrit le logiciel (et il est
  déclaratif), `Mcp-Session-Id` n'existe plus. Le seul mécanisme prévu par la spec est
  `Authorization: Bearer` (OAuth 2.1), la passerelle jouant le rôle de *resource server*.
- **Une limite par adresse IP ne marche pas avec Claude.ai** : les connecteurs appellent depuis
  les serveurs d'Anthropic, donc tous leurs utilisateurs partageraient quelques adresses.
- Options envisagées, de la plus simple à la plus complète :

  | Mode | Identité | Remarque |
  | --- | --- | --- |
  | `api_key` | clé personnelle en `Authorization: Bearer` | simple, sans fournisseur d'identité ; clés dans un Secret Kubernetes |
  | `oauth` | jeton d'un fournisseur d'identité (Keycloak, éventuellement fédéré avec GitHub ou Google) | le plus propre ; l'utilisateur se connecte dans son navigateur. Limite : il faut un compte sur ce fournisseur |

- **Ne jamais retransmettre le jeton d'un utilisateur aux amonts** (interdit par la spec,
  risque de « confused deputy ») : si un amont exige une authentification, la passerelle
  utilise ses propres identifiants.

### Recherche avec le crate `bm25`

Il y a au plus quelques centaines d'outils : l'index se reconstruit en quelques millisecondes
et tient en mémoire. Un moteur complet (tantivy) ou une base embarquée (DuckDB et son
extension plein texte) seraient surdimensionnés. Le module est isolé derrière le trait
`ToolIndex` en vue d'une contribution au SDK rmcp (module optionnel ou crate séparé, à
discuter d'abord avec ses mainteneurs).

### Catalogue en ressources plutôt qu'en outils

Une ressource ne coûte rien au contexte du client tant qu'il ne la lit pas, et elle reste
lisible quand la recherche d'outils masque `tools/list`. Le catalogue est toujours généré
depuis le registre et l'état des amonts ; le guide de déploiement contient du récit et des
renvois, jamais d'inventaire.

### Métriques Prometheus en plus des logs

Les logs JSON vont dans Loki, mais calculer des latences et des taux d'erreur à partir des
logs est coûteux et imprécis. Les métriques Prometheus, servies sur un port séparé non exposé
publiquement, alimentent les tableaux de bord Grafana à faible coût.

### Transport amont `sse` non supporté

L'ancien transport HTTP+SSE est déprécié par la spec et n'existe plus dans le client
rmcp 3.x. La valeur reste acceptée dans le registre pour ne pas casser une config existante,
mais le serveur est signalé `unavailable` avec un message explicite.

### Mesure de l'overhead

Mesuré en local avec `cargo bench --bench overhead` :

| Chemin | Temps médian |
| --- | --- |
| client → amont | ~0,95 ms |
| client → passerelle → amont | ~1,8 ms |
| `search_tools` (sans amont) | ~0,87 ms |

L'overhead (~0,9 ms) vient essentiellement du saut HTTP supplémentaire ; le traitement propre
à la passerelle (résolution, validation, sémaphores) est négligeable. Les réponses en JSON
simple plutôt qu'en flux SSE (`json_response`) l'ont réduit d'environ 20 %.

### Récapitulatif des choix écartés

| Option | Pourquoi pas (pour l'instant) |
| --- | --- |
| Rechargement à chaud de la config | inutile sans session : un rolling update ne coupe personne |
| Sessions MCP | supprimées par la spec 2026-07-28 ; elles imposeraient des sessions sticky |
| tantivy / DuckDB pour la recherche | surdimensionnés pour quelques centaines d'outils |
| Authentification, limites par utilisateur | reportées après la V1 (voir plus haut) |
| Limite par adresse IP | inopérante avec les connecteurs Claude.ai |
| Transport amont `sse` | déprécié par la spec et absent du client rmcp 3.x |
