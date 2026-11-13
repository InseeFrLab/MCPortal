# Rapport de review MCPortal v0.1.0

Périmètre : tout `src/`. Clippy ne remonte aucun avertissement et les tests passent (22 unitaires, 15 d'intégration). La base est saine, mais la review a trouvé un bug de disponibilité sérieux et quelques problèmes réels, triés ci-dessous par priorité.

## P0 : à corriger avant la mise en production

### 1. ✅ Corrigé : un upstream injoignable fait attendre les appels l'un après l'autre (`upstream.rs`)

> **Résolution** : les trois correctifs ci-dessous sont appliqués. La connexion est lue sans verrou (`ArcSwapOption`), et un mutex ne sert plus qu'à sérialiser les tentatives de connexion. Un échec de connexion est mémorisé 5 s (`RECONNECT_COOLDOWN`) : les appels en attente et ceux qui arrivent pendant ce délai échouent immédiatement avec « next connection attempt in Ns ». Le `timeout` couvre maintenant toute l'opération, attente de connexion comprise. Le test `hanging_upstream_does_not_queue_calls` reproduit le cas (upstream qui accepte TCP sans jamais répondre) : 3 appels simultanés prenaient 3,1 s avec l'ancien code, ils prennent environ 1 s désormais.

`connection()` garde le `tokio::sync::Mutex` pendant tout `connect()`, qui peut durer jusqu'à `timeout` (30 s par défaut). Dans `call_tool`, `self.connection().await` est appelé **avant** le `tokio::time::timeout`, donc l'attente du verrou n'est pas bornée par le timeout de l'appel.

**Scénario** : l'upstream tombe (DNS ou TCP qui ne répond pas) et 4 appels arrivent (`max_concurrency: 4`). Le 1ᵉʳ essaie de se connecter pendant 30 s et échoue. Le 2ᵉ prend alors le verrou et retente 30 s, et ainsi de suite. Le 4ᵉ appel répond au bout d'environ 120 s, en gardant pendant tout ce temps un permis global et un permis serveur. Le timeout annoncé à l'utilisateur n'est donc pas respecté. Le même mécanisme peut aussi dépasser `terminationGracePeriodSeconds` lors d'un rolling update.

**Correctifs** :

- placer l'obtention de la connexion **à l'intérieur** du `timeout` ;
- ne pas retenter la connexion à chaque appel quand une tentative vient d'échouer : mémoriser l'échec avec un horodatage et renvoyer immédiatement `Unavailable` pendant N secondes, comme un disjoncteur simple ;
- en option, utiliser `ArcSwapOption<Connection>` pour la lecture rapide. Le `Mutex` ne servirait plus qu'à la (re)connexion, et le chemin chaud n'aurait plus de verrou async.

### 2. ✅ Corrigé : `invalidate()` peut fermer une connexion saine toute neuve (`upstream.rs`)

> **Résolution** : `invalidate` reçoit la connexion utilisée et ne la retire que si elle est toujours la connexion courante (`compare_and_swap`).

`map_result` appelle `invalidate()`, qui retire **la connexion courante**, sans vérifier que c'est celle qui a échoué.

**Scénario** : les appels A et B échouent sur l'ancienne connexion. A invalide, C se reconnecte, puis B invalide à son tour la connexion de C. Sous charge, on obtient des cascades de reconnexions, et donc des `initialize` ou `server/discover` répétés.

**Correctif** : passer l'`Arc<Connection>` utilisé à `invalidate` et ne retirer la connexion que si `Arc::ptr_eq(current, used)`.

### 3. Des noms de tools choisis par le client deviennent des labels Prometheus (`gateway.rs:287`, `402`)

Pour un tool inconnu, `record_call("unknown", name, "invalid", …)` met le `name` fourni par le client (souvent un nom halluciné par le LLM, sans limite de taille ni de variété) dans le label `tool` de `mcportal_tool_calls_total`. L'endpoint étant public et non authentifié, n'importe qui peut créer des séries à volonté. Cela contredit le principe d'ARCHITECTURE.md, qui veut éviter l'explosion de cardinalité.

**Correctif** : utiliser `tool = "unknown"` dans la métrique et garder le vrai nom seulement dans le log.

## P1 : important

### 4. ✅ Corrigé : l'ordre « global puis serveur » produit l'effet inverse de celui documenté (`limits.rs`)

> **Résolution** : le sémaphore global est supprimé plutôt que réordonné. Il ne protégeait pas la gateway (un appel en cours coûte une tâche tokio, et le total est déjà borné par la somme des `max_concurrency`). Sa seule conséquence était de coupler les serveurs entre eux. La clé `concurrency.global_limit` reste acceptée dans le registre, ignorée avec un avertissement au démarrage. La justification est consignée dans ARCHITECTURE.md (*Design decisions*).

Un appel vers un serveur saturé prend d'abord un permis **global**, puis attend son permis serveur jusqu'à `acquire_timeout` (5 s). Avec `global_limit: 16` et `max_concurrency: 4`, 12 appels en attente vers un serveur lent bloquent les 12 permis globaux restants, et tous les autres serveurs sont refusés pendant ce temps. C'est exactement le cas que le commentaire dit éviter.

**Correctif** : acquérir le permis serveur d'abord, puis le permis global. L'attente d'un permis serveur ne pénalise alors que ce serveur. L'ordre reste constant, donc il n'y a pas de risque d'interblocage, et le timeout est déjà partagé. Corriger aussi le doc-comment et ARCHITECTURE.md.

### 5. ✅ Corrigé : fuite mémoire dans `ProgressRouter.early` (`upstream.rs`)

> **Résolution** : le tampon `early` a été supprimé. Une notification sans route est désormais ignorée. La course qu'il couvrait (notification arrivée entre l'envoi et `register`) dure quelques microsecondes, contre un aller-retour HTTP, et perdre une notification purement informative ne casse rien. La fuite et le risque d'inversion d'ordre disparaissent avec lui. ARCHITECTURE.md est mis à jour en conséquence.

rmcp attache un `progressToken` à **chaque** requête, y compris sur le chemin `call_tool_once` qui n'enregistre jamais de route. Tout upstream qui émet des notifications de progression pour un appel sans token côté client voit ces notifications stockées dans `early[token]`, puis supprimées seulement quand on atteint 256 tokens distincts. Le `Vec` d'un même token n'a pas de taille maximale : un tool long qui émet 10 000 notifications les garde toutes en mémoire.

**Correctif** : limiter aussi la taille du `Vec` (par exemple 8 éléments, on peut garder seulement la dernière notification). Mieux encore, ne mettre en tampon que les tokens attendus : il faudrait les déclarer comme « en attente » avant l'envoi, ce que l'API rmcp ne permet sans doute pas, d'où le plafond.

**Point secondaire** : le `tokio::spawn` dans `register` qui renvoie les notifications arrivées en avance peut s'entrelacer avec les `forward` directs. La progression peut alors reculer côté client, alors que la spec exige qu'elle soit croissante.

### 6. Un appel réussi ne remet pas à jour la santé du serveur (`upstream.rs:409-431`)

ARCHITECTURE.md indique qu'un appel tenté sur un upstream `unavailable` permet de le récupérer. La connexion est bien rouverte, mais `consecutive_failures` n'est remis à zéro que par `record_probe`. Le statut affiché dans `catalog://servers` et la métrique `mcportal_upstream_up` restent donc à `unavailable` jusqu'à la sonde suivante (60 s), alors que les appels fonctionnent.

**Correctif** : remettre `consecutive_failures` à zéro après un succès dans `call_tool`, si c'est bien le comportement voulu (voir Q1).

## P2 : performance et propreté (impact faible, à faire quand c'est pratique)

7. **Clones inutiles sur le chemin chaud** (`gateway.rs:470-486`). `request.arguments.clone()` copie en profondeur le JSON des arguments à chaque appel. Il vaut mieux déstructurer `request` par valeur et ne garder que `input_responses` et `request_state`.
8. **Validation** (`gateway.rs:294-318`). Appeler `validator.is_valid()` avant `iter_errors()`, qui est plus coûteux, et ne collecter les erreurs qu'en cas d'échec. Le motif `Value::Object(arguments)` suivi de `let … else { unreachable!() }` peut aussi disparaître si `forward` reçoit directement un `Value`.
9. **`rebuild_catalog` recompile tous les validateurs** (`state.rs:136-182`) à chaque reconstruction, y compris ceux des upstreams inchangés. Au démarrage, `initial_load` lance une reconstruction par upstream : avec N upstreams, on compile environ N² × (tools par upstream) validateurs. Deux solutions : une seule reconstruction après le `join_all`, et/ou un cache des validateurs par upstream à côté de `ToolCache`.
10. **`list_tools` reconstruit toute la liste à chaque requête** (`gateway.rs:441-447`). Les schémas `schema_for_type` sont régénérés et, en mode `tool_search.enabled: false`, chaque `Tool` avec son schéma est cloné. On peut précalculer un `Arc<[Tool]>` dans `ToolCatalog` et les tools propres de la gateway dans un `LazyLock`.
11. **Validations de config manquantes** (`config.rs:110`). Trois valeurs font planter le démarrage avec une panique au lieu d'un message clair :
    - `acquire_timeout_seconds: .inf` : `Duration::from_secs_f64` panique ;
    - `timeout` très grand : même panique ;
    - `http.path: "/"` ou un `health.path` égal à `/ready` ou à `http.path` : axum panique sur `nest_service` ou à cause du conflit de routes.

    Il suffit d'ajouter ces cas à `validate()`.
12. **Commentaire de `telemetry.rs:33`** : le statut `cancelled` manque dans la liste.

## Questions à clarifier

- **Q1 : santé.** Un upstream doit-il redevenir `available` dès qu'un appel réussit, ou seulement à la sonde suivante ? Cela décide du correctif du point 6.
- **Q2 : `_meta` du client.** `forward` construit un `CallToolRequestParams` neuf : le `_meta` downstream (par exemple `traceparent` ou d'autres clés) et le paramètre `task` ne sont pas transmis. Est-ce volontaire ?
- **Q3 : tâches (SEP-2663).** Si un upstream renvoie un `CreateTaskResult` (tool avec `taskSupport: required`), la gateway le relaie tel quel. Mais elle n'expose ni `tasks/get` ni `tasks/result`, donc le client ne peut pas suivre la tâche. Faut-il exclure ces tools du catalogue, ou est-ce hors périmètre pour la V1 ?
- **Q4 : longueur et caractères des noms exposés.** En mode `tool_search.enabled: false`, `{server}_{tool}` peut dépasser 64 caractères ou contenir des caractères que certains clients refusent (Claude impose `^[a-zA-Z0-9_-]{1,64}$`). Un seul tool non conforme peut alors faire rejeter tout `tools/list`. Faut-il filtrer ou tronquer ces noms et le signaler dans un log ?
- **Q5 : timeout et progression.** La gateway relaie les `notifications/progress` vers le client, mais ne s'en sert pas pour son propre timeout : l'appel upstream est enveloppé dans un `tokio::time::timeout` fixe (`upstream.rs:427`) et part avec `PeerRequestOptions::no_options()`. Un tool long qui émet régulièrement sa progression est donc coupé par la gateway au bout de `timeout` (30 s par défaut), alors qu'un client avec `resetTimeoutOnProgress` aurait attendu. Des tools approchent-ils ou dépassent-ils 30 s (requêtes SPARQL lourdes, grosses extractions MELODI) ? Si oui, deux options :
  - augmenter le `timeout` des upstreams concernés : c'est simple, mais une panne franche met plus longtemps à être détectée ;
  - remettre le timeout à zéro à chaque notification (`PeerRequestOptions { timeout, reset_timeout_on_progress: true }` dans rmcp), en gardant un plafond absolu.
- **Q6 : messages d'erreur.** Les erreurs d'upstream (`Unavailable { reason }`) sont renvoyées au LLM et publiées dans `catalog://servers`. Elles peuvent contenir des URLs ou des noms d'hôtes internes au cluster. Est-ce acceptable pour un endpoint public ?

## Vérifié, rien à signaler

- **Limite de taille des requêtes HTTP** : rmcp applique déjà `max_request_body_bytes` par défaut.
- **Annulation** : quand un appel est abandonné, `call_tool_once` (`send_request_cancel_on_drop`) et `CancelOnDrop` envoient bien `notifications/cancelled`.
- **Arrêt propre** : la séquence d'arrêt, le publish atomique via `ArcSwap` avec `rebuild_lock`, l'ordre des verrous `health` puis `tools` (pas d'interblocage) et les `std::Mutex` sans `await` sont corrects.

## Ordre conseillé

1. Points 1 et 2 ensemble, puisqu'ils touchent le même code de connexion.
2. Puis 3, 4, 5.
3. Le reste peut attendre une itération suivante.
