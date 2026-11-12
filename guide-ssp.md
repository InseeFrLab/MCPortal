# Démarches types — passerelle SSP (INSEE + IGN)

Ce guide complète le guide générique servi dans `catalog://guide`. Il décrit les serveurs
exposés par cette installation, le routage entre briques et des démarches concrètes. Les noms
d'outils ci-dessous sont les noms à appeler dans la passerelle : le préfixe est le nom du
serveur (`<serveur>_<outil>`). L'inventaire courant fait foi :
`catalog://servers/{nom}/tools`.

## Une famille INSEE, trois serveurs

`insee`, `melodi` et `rmes` sont trois serveurs indépendants de la même famille de données
statistiques de l'INSEE. Ils partagent le domaine `Insee`, mais chacun a son propre préfixe et
son propre inventaire. Le serveur `mcpdiffusion` est l'agrégat qui rassemble ces sources ; ce
n'est pas une quatrième brique de données. Les démarches de ce guide utilisent les préfixes
directs des trois briques pour rendre le routage visible.

| Serveur | Domaine | À choisir pour |
| --- | --- | --- |
| `insee` | `Insee` | Publications, Informations rapides, chiffres clés, conjoncture et indicateurs de la page d'accueil insee.fr |
| `melodi` | `Insee` | Catalogue de jeux de données statistiques, modalités et observations (notamment les séries historiques granulaires) |
| `rmes` | `Insee` | Métadonnées statistiques : définitions, vocabulaires, nomenclatures et classifications via SPARQL — pas les chiffres |
| `geodata` | `geographie` | Données géographiques de l'IGN : géocodage, découpage administratif, cadastre, urbanisme, altitude et WFS Géoplateforme |

`geodata` n'expose que des outils : il n'implémente ni `prompts/list` ni `resources/list`
(réponse `Method not found`). C'est une limite du serveur amont, pas de la passerelle.

## Routage entre les briques INSEE

- **Publication ou analyse rédigée** : cherchez dans `insee` avec
  `insee_search_insee_documents`, puis lisez le résultat avec `insee_get_insee_document`.
  Pour un chiffre ponctuel, préférez `insee_search_insee_chiffrecle` ; pour un indicateur
  récent ou une question générique sur les indicateurs, utilisez `insee_get_insee_homepage`
  ou `insee_search_insee_conjoncture` selon le besoin.
- **Série historique granulaire ou observations** : passez à `melodi`, en commençant par
  `melodi_search_melodi_datasets`, puis `melodi_search_melodi_modalities` et
  `melodi_get_melodi_observations`. La recherche de jeux de données est lexicale : formulez
  une requête explicite en français. Ne devinez jamais un identifiant opaque ni un code de
  modalité : reprenez les valeurs retournées par les outils de recherche.
- **Définition, ontologie, vocabulaire ou classification** : passez à `rmes`. Explorez avec
  `rmes_search_rmes_graphs`, interrogez avec `rmes_run_rmes_sparql` et, pour le détail d'une
  ressource, utilisez `rmes_describe_rmes_resource`. RMES répond en métadonnées seulement,
  pas en chiffres ni en observations.

En particulier, les descriptions de publications orientent vers RMES pour les définitions,
les codes et les classifications, et vers MELODI pour les séries historiques fines. Les
chiffres clés ne remplacent pas les observations granulaires de MELODI. À l'inverse, MELODI
n'est pas le bon point d'entrée pour un indicateur générique actualisé, l'analyse en texte
intégral d'un rapport publié, ni une définition ou une ontologie : utilisez respectivement
les outils insee.fr ou RMES indiqués ci-dessus. Si une observation MELODI nécessite une
explication de concept ou un vocabulaire de codes, complétez-la avec RMES.

## Démarche 1 — Portrait d'un territoire

Le point clé : `geodata` travaille en coordonnées (`lon`/`lat`), les statistiques INSEE en
nom de territoire (`geo_keyword` + `geo_level`). Le géocodage est la porte d'entrée :

1. `geodata_geocode(text="Beauvais")` → renvoie un libellé, un type et des coordonnées
   `lon`/`lat` réutilisables dans les outils géographiques.
2. `insee_search_insee_chiffrecle(query="population, logement, emploi",
   geo_level="COM", geo_keyword="Beauvais")` → tableaux synthétiques prêts à l'emploi.
   `geo_level` accepte `COM`, `DEP`, `REG`, `INTER`, `COMPRD`, `FRANCE`.
3. Pour le contexte géographique au même point : `geodata_adminexpress(lat=…, lon=…)` (unités
   administratives), `geodata_cadastre(lat=…, lon=…)`, `geodata_urbanisme(lat=…, lon=…)` et
   `geodata_altitude(lat=…, lon=…)`.

Toutes les réponses sont des données ; c'est vous qui composez la synthèse.

## Démarche 2 — Chercher une publication puis la lire

1. `insee_search_insee_documents(query="effets du covid sur le tourisme",
   geo_level="REG", geo_keyword="Occitanie", year_of_reference=2024)` — formulez la
   requête en langage naturel avec des synonymes et le contexte (année, géographie).
2. `insee_get_insee_document(document_urls=["<url du résultat>"])` → texte complet.
3. Pour un chiffre ponctuel plutôt qu'une analyse rédigée, utilisez
   `insee_search_insee_chiffrecle`. Pour un indicateur mensuel récent, utilisez
   `insee_search_insee_conjoncture`.

## Démarche 3 — Explorer une série statistique (MELODI)

Parcourez MELODI dans cet ordre :

1. `melodi_search_melodi_datasets(query="indice des prix à la consommation",
   start_year=2015)` → jeux de données candidats et leurs identifiants. Écrivez la requête
   en français explicite (par exemple « décès par département » ou « prénoms des
   nouveau-nés »).
2. `melodi_search_melodi_modalities(dataset_id=…, column_ids=["GEO"], query="…")`
   → codes de modalité valides. Reprenez les codes retournés ; n'inventez pas de filtre.
3. `melodi_get_melodi_observations(dataset_id=…, years=[2020, 2021],
   column_filters={"GEO": "<code modalité>"}, number_of_observations=…)` → observations,
   dimensions, unités et mesures.

Pour expliquer un concept ou un code utilisé par la série, complétez la recherche avec les
outils `rmes` décrits plus haut ; RMES apporte le sens et les métadonnées, pas les valeurs.

## Démarche 4 — Une question, les trois briques

Question : « Comment a évolué le prix à la consommation, quels jeux de données permettent
le vérifier, et que signifie la nomenclature utilisée ? » Suivez le passage publications →
données → métadonnées et transmettez les identifiants renvoyés à l'étape suivante :

1. `insee_search_insee_documents(query="évolution des prix à la consommation",
   year_of_reference=2024)` → repérez une publication ou analyse pertinente. Si vous en
   avez besoin, lisez-la avec `insee_get_insee_document(document_urls=["<url du résultat>"])`.
2. `melodi_search_melodi_datasets(query="indice des prix à la consommation",
   start_year=2015)` → repérez un jeu de données ; prenez son `dataset_id` dans le résultat.
   Cherchez ensuite les codes nécessaires avec
   `melodi_search_melodi_modalities(dataset_id=…, column_ids=["GEO"], query="France")`,
   puis lisez les observations utiles avec `melodi_get_melodi_observations` en réutilisant
   l'identifiant et les codes retournés.
3. `rmes_search_rmes_graphs(graph_category="produits")` → repérez le graphe ou la ressource
   de métadonnées pertinente (choisissez `nomenclatures` pour une nomenclature). Utilisez
   `rmes_run_rmes_sparql` avec une requête filtrée sur le ou les graphes découverts, puis
   `rmes_describe_rmes_resource` si le détail d'une ressource identifiée est nécessaire.

Les trois réponses sont complémentaires : la publication donne le contexte éditorial,
MELODI les séries chiffrées et RMES le sens des concepts et codes. Les résultats de recherche
peuvent être vides ; dans ce cas, reformulez ou changez de brique selon le besoin, sans
fabriquer d'identifiant, de code ni de résultat.

## Démarche 5 — Données WFS de la Géoplateforme

1. `geodata_gpf_wfs_search_types(query="…")` → types disponibles.
2. `geodata_gpf_wfs_describe_type(typename=…)` → schéma (identifiants et attributs).
3. `geodata_gpf_wfs_get_features(typename=…, spatial_operator="bbox",
   bbox_west=…, bbox_south=…, bbox_east=…, bbox_north=…)` ou
   `geodata_gpf_wfs_get_feature_by_id(typename=…, feature_id=…)`. Pour un filtre
   attributaire, renseignez `where` avec une propriété trouvée à l'étape de description.

Pas besoin d'écrire du CQL : les filtres attributaires et spatiaux sont des paramètres.

## Rappels opérationnels

- Découverte : `catalog://servers`, `catalog://domains`, puis
  `catalog://servers/{nom}/tools`, ou directement les inventaires documentés ici :
  `catalog://servers/insee/tools`, `catalog://servers/melodi/tools`,
  `catalog://servers/rmes/tools`, `catalog://servers/mcpdiffusion/tools` et
  `catalog://servers/geodata/tools`. La recherche d'outils (si activée) porte sur le texte
  des outils — « melodi », « cadastre », « insee » fonctionnent, un mot de domaine comme
  « diffusion » non.
- Un amont peut être temporairement indisponible : `catalog://servers` porte son état ;
  `/health` (HTTP) donne l'état détaillé et les métriques.
- Retour opérateur : appelez `send_feedback(server="insee", author="…",
  feedback="…")` pour signaler un problème concernant une brique ou un serveur. Le nom doit
  être celui d'un serveur du registre ; le rapport est consigné dans le journal de la passerelle,
  sans dépendre d'un service amont.
