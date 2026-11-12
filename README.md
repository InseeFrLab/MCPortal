# MCPortal

Ce Portail MCP agrège plusieurs serveurs MCP déclarés dans un
registre YAML et les expose à un client MCP (Claude, Claude Code…) sous la forme d'**un
seul serveur**. 

```text
client MCP ──HTTP──▶ MCPortal ──HTTP──▶ serveur MCP « insee »
                                 ├────▶ serveur MCP « melodi »
                                 └────▶ serveur MCP « geodata »
```

Plutôt que de charger des dizaines d'outils dans le contexte du client, la passerelle
expose trois outils :

| Outil | Rôle |
| --- | --- |
| `search_tools(query, limit?)` | recherche plein texte (BM25) parmi les outils de tous les serveurs ; renvoie leur nom et leur `inputSchema` |
| `call_tool(name, arguments)` | appelle un outil amont par son nom complet `{serveur}_{outil}` |
| `send_feedback(server, author, feedback)` | consigne un retour dans les logs (label `FEEDBACK`) |

Elle expose aussi un **catalogue en ressources** (`catalog://servers`, `catalog://domains`,
`catalog://servers/{nom}`, `catalog://servers/{nom}/tools`, `catalog://guide`), lisible
sans consommer de contexte.

## Fonctionnalités

- Protocole MCP **2026-07-28** (sans session), compatible avec les clients plus anciens.
- Mode **stateless** : plusieurs réplicas sans sessions sticky.
- Validation des arguments contre le JSON Schema de l'outil cible, avant tout appel amont.
- Relais transparent des résultats, de la progression, de l'annulation et des échanges
  multi-tours (`input_required`).
- Limites de concurrence globale et par serveur, avec délai d'attente borné.
- Sondes de santé des amonts ; un amont en panne ne casse ni le catalogue ni les autres.
- Logs JSON sur la sortie standard (Loki / Grafana) et métriques Prometheus.
- `/health` (liveness), `/ready` (readiness) et arrêt propre pour Kubernetes.

## Démarrage rapide

Prérequis : Rust 1.88 ou plus récent (édition 2024). Aucune dépendance système : le TLS
passe par rustls, sans OpenSSL.

```bash
cargo run -- --config registry.yaml
```

| Port | Routes |
| --- | --- |
| 8000 | `/mcp` (endpoint MCP), `/health`, `/ready` |
| 9090 | `/metrics` (Prometheus) |

Pour brancher la passerelle dans Claude Code :

```bash
claude mcp add --transport http mcportal http://localhost:8000/mcp
```

## Configuration

Toute la configuration tient dans le registre YAML (chemin passé par `--config` ou la
variable `MCPORTAL_CONFIG`). Exemple :

```yaml
gateway:
  name: SSP Gateway
  tool_search: {enabled: true, max_results: 5}
  catalog: {enabled: true, include_tool_inventory: true, include_guide: true, guide_path: guide-ssp.md}
  health: {enabled: true, path: /health, probe_interval_seconds: 60, failure_threshold: 2}
  metrics: {log_level: INFO, json_logs: true, prometheus: {enabled: true, port: 9090}}
  concurrency: {global_limit: 16, acquire_timeout_seconds: 5}
  http: {bind: "0.0.0.0:8000", path: /mcp, allowed_hosts: []}

servers:
  - name: mcpdiffusion            # nom unique, sert aussi de préfixe d'outils
    url: https://mcpdiffusion.lab.sspcloud.fr/mcp
    transport: http               # http (sse : déprécié, non supporté)
    domain: diffusion
    description: Serveur pour exposer Melodi, Rmes et insee.fr
    tags: [dataset, publications] # liste ou chaîne unique
    enabled: true                 # false : visible dans le catalogue, non exposé
    timeout: 30.0                 # délai par appel amont (s)
    max_concurrency: 4            # appels simultanés vers ce serveur
    cache_ttl: 300                # durée de vie du cache de la liste d'outils (s)
```

Chaque clé a une valeur par défaut ; elles sont toutes documentées dans
[`src/config.rs`](src/config.rs). Une clé inconnue (faute de frappe) ou une valeur
incohérente empêche le démarrage, avec un message explicite.

`guide_path` désigne le guide propre au déploiement : il est **relatif au registre**,
vérifié au démarrage et relu à chaque lecture de `catalog://guide`.

La variable `RUST_LOG` est prioritaire sur `log_level` (ex. `RUST_LOG=mcportal=debug`).

## Déploiement

```bash
docker build -t mcportal:0.1.0 .
```

Un exemple Kubernetes (ConfigMap, Deployment, Service) est fourni dans
[`deploy/kubernetes.yaml`](deploy/kubernetes.yaml). Points clés :

- le registre et le guide sont montés depuis un ConfigMap dans `/config` ;
- un changement de config se déploie par **rolling update** (pas de rechargement à chaud) ;
- `terminationGracePeriodSeconds` doit dépasser le plus grand `timeout` amont ;
- le port 9090 (`/metrics`) ne doit pas être exposé par l'ingress.

## Tests et mesures

```bash
cargo test                                   # tests unitaires et d'intégration (hors réseau)
cargo test --test guide_drift -- --ignored   # cohérence du guide avec le registre et les vrais amonts
cargo bench --bench overhead                 # overhead de la passerelle
```

Les tests d'intégration démarrent de vrais serveurs MCP simulés sur des ports aléatoires :
ils n'ont besoin d'aucun service externe.

Overhead mesuré en local : environ **0,9 ms** par appel relayé, dû essentiellement au saut
HTTP supplémentaire. En production, la métrique `mcportal_gateway_overhead_seconds` donne la
même mesure.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) : organisation du code, fonctionnement interne et
  décisions de conception.
- [CONTRIBUTING.md](CONTRIBUTING.md) : comment contribuer.
- [`src/guide.md`](src/guide.md) : guide d'utilisation servi aux clients MCP.

## Limites connues (V1)

- Pas d'authentification ni de limite par utilisateur.
- Pas de rechargement à chaud de la configuration.
- Transport amont `sse` non supporté (déprécié par la spec MCP).

Les raisons de ces choix sont détaillées dans
[ARCHITECTURE.md](ARCHITECTURE.md#décisions-de-conception).

## Contribuer

Les contributions sont les bienvenues : signalement de bug, idée d'évolution, documentation
ou code.

1. Ouvrez une issue pour décrire le bug ou discuter de l'évolution avant de vous lancer.
2. Créez une branche à partir de `main` (`feat/…`, `fix/…`, `docs/…`).
3. Vérifiez que tout passe :

   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

4. Ouvrez une pull request vers `main`.

Le détail (conventions de code, messages de commit, tests, liste de contrôle) est dans
[CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

Distribué sous licence [MIT](LICENSE).
