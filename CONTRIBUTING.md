# Contribuer à MCPortal

Merci de votre intérêt pour le projet. Ce document explique comment préparer
l'environnement, proposer une modification et la faire accepter.

Avant de modifier le code, lisez [ARCHITECTURE.md](ARCHITECTURE.md) : il décrit les modules
et les principes à respecter.

## Signaler un problème ou proposer une idée

Ouvrez une issue en indiquant :

- **pour un bug** : la version (ou le commit), le registre utilisé (sans secret), ce que vous
  avez fait, ce que vous attendiez, ce qui s'est passé, et les logs utiles ;
- **pour une évolution** : le besoin et le cas d'usage, avant la solution envisagée.

Pour une évolution importante, discutez-en dans une issue **avant** d'écrire du code : cela
évite de travailler sur une piste qui ne serait pas retenue.

Ne publiez pas de faille de sécurité dans une issue publique : contactez directement les
mainteneurs.

## Préparer l'environnement

- Rust 1.88 ou plus récent (`rustup update stable`), avec `rustfmt` et `clippy`.
- Aucune dépendance système n'est nécessaire (TLS via rustls).

```bash
git clone <url-du-dépôt>
cd MCPortal
cargo build
cargo test
```

Pour lancer la passerelle localement : `cargo run -- --config registry.yaml`.

## Proposer une modification

1. Créez une branche à partir de `main`, nommée d'après son objet :
   `feat/limite-par-utilisateur`, `fix/timeout-reconnexion`, `docs/guide-geodata`.
2. Faites des commits petits et cohérents.
3. Vérifiez localement que tout passe (voir ci-dessous).
4. Ouvrez une pull request vers `main` en remplissant la liste de contrôle.

### Messages de commit

Le projet suit [Conventional Commits](https://www.conventionalcommits.org/fr/) :

```text
<type>(<portée facultative>): <résumé à l'impératif>

<corps facultatif : le pourquoi, pas le comment>
```

Types usuels : `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `chore`, `ci`.
Exemples :

```text
feat(search): indexer les descriptions des paramètres
fix(upstream): reconnecter après une session expirée
docs: documenter la clé gateway.http
```

### Vérifications avant la pull request

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Si vous touchez au chemin d'un appel (`gateway.rs`, `upstream.rs`, `limits.rs`, `state.rs`),
lancez aussi le benchmark et signalez toute régression dans la pull request :

```bash
cargo bench --bench overhead
```

Si vous modifiez `registry.yaml` ou `guide-ssp.md`, lancez les tests de dérive du guide (ils
interrogent les vrais serveurs amont) :

```bash
cargo test --test guide_drift -- --ignored
```

### Liste de contrôle de la pull request

- [ ] Les vérifications ci-dessus passent.
- [ ] Le comportement nouveau ou corrigé est couvert par un test.
- [ ] Les commentaires et la documentation sont à jour (README, ARCHITECTURE, `src/config.rs`).
- [ ] Une nouvelle clé de configuration a une valeur par défaut, une validation si besoin, et
      apparaît dans le README.
- [ ] Une nouvelle métrique est déclarée dans `telemetry::metric` et listée dans ARCHITECTURE.

## Conventions de code

### Style

- Formatage par `rustfmt` (configuration par défaut), aucun avertissement `clippy`.
- Commentaires et messages en **français**.
- Chaque module commence par un commentaire `//!` qui explique son rôle et ses choix ; les
  éléments publics ont un commentaire `///`. Commentez le **pourquoi** quand il n'est pas
  évident, pas ce que le code dit déjà.
- Les commentaires `///` des structures d'arguments d'outils (`SearchToolsArgs`…) deviennent
  les descriptions vues par le LLM : rédigez-les pour lui.

### Principes à respecter

- **Simplicité** : préférez la solution la plus simple qui répond au besoin ; une
  abstraction doit se justifier par un usage réel.
- **Rien de bloquant** sur le chemin des requêtes ; pas d'`await` en tenant un
  `std::sync::Mutex`.
- **Pas de seconde source de vérité** : le catalogue et les instructions sont générés depuis
  le registre et les amonts, jamais écrits à la main.
- **Erreurs** : ce que le LLM peut corriger devient un `CallToolResult` avec
  `isError: true` et un message actionnable ; les erreurs JSON-RPC sont réservées aux requêtes
  mal formées.
- **Résultats amont relayés tels quels** : ne réemballez pas `content` ni
  `structuredContent`.
- **Métriques** : jamais d'identifiant d'utilisateur ni de valeur non bornée en label.
- **Dépendances** : toute nouvelle dépendance doit être justifiée dans la pull request.

### Tests

- Tests unitaires dans le module concerné (`#[cfg(test)] mod tests`).
- Tests de bout en bout dans `tests/`, avec les outils de `tests/common/mod.rs` :
  `spawn_upstream` (serveur MCP simulé), `spawn_gateway`, `connect`, `call_via_gateway`.
  Pour un nouveau comportement amont, ajoutez un outil au `MockUpstream`.
- Les tests n'utilisent pas le réseau externe ; ceux qui en ont besoin sont marqués
  `#[ignore = "raison"]`.
- Les tests utilisent des ports aléatoires (`127.0.0.1:0`) : ils peuvent tourner en parallèle.

## Cas fréquents

### Ajouter une clé de configuration

1. Ajoutez le champ dans la structure concernée de `src/config.rs`, avec
   `#[serde(default = …)]` et un commentaire `///`.
2. Mettez à jour l'implémentation `Default` de la structure.
3. Ajoutez la validation dans `Config::validate` si la valeur peut être incohérente.
4. Ajoutez un test unitaire dans `config.rs`.
5. Documentez la clé dans le README.

### Modifier le guide

- `src/guide.md` (mécanique) ne doit citer **aucun** serveur ni outil d'un déploiement ; le test
  `mechanics_guide_cites_no_deployment_server` le vérifie.
- `guide-ssp.md` (déploiement) contient du récit et des renvois, jamais d'inventaire : celui-ci
  est généré par le catalogue.

## Licence

Le projet est distribué sous licence [MIT](LICENSE). En proposant une contribution, vous
acceptez qu'elle soit distribuée sous cette même licence.
