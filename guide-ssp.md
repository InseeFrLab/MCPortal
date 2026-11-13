# Typical workflows — SSP gateway (INSEE + IGN)

This guide complements the generic guide served in `catalog://guide`. It describes the
servers exposed by this installation, routing between components and concrete workflows.
The tool names below are the names to call through the gateway: the prefix is the server
name (`<server>_<tool>`). The current inventory is authoritative:
`catalog://servers/{name}/tools`.

The underlying data is French: write search queries in French, as in the examples below.

## One INSEE family, three servers

`insee`, `melodi` and `rmes` are three independent servers from the same family of INSEE
statistical data. They share the `Insee` domain, but each has its own prefix and its own
inventory. The `mcpdiffusion` server is the aggregate that brings these sources together;
it is not a fourth data component. The workflows in this guide use the direct prefixes of
the three components to make routing visible.

| Server | Domain | Choose it for |
| --- | --- | --- |
| `insee` | `Insee` | Publications, rapid releases (Informations rapides), key figures, economic outlook and insee.fr homepage indicators |
| `melodi` | `Insee` | Catalog of statistical datasets, modalities and observations (notably granular historical series) |
| `rmes` | `Insee` | Statistical metadata: definitions, vocabularies, nomenclatures and classifications via SPARQL — not figures |
| `geodata` | `geographie` | IGN geographic data: geocoding, administrative boundaries, cadastre, urban planning, altitude and Géoplateforme WFS |

`geodata` only exposes tools: it implements neither `prompts/list` nor `resources/list`
(`Method not found` response). This is a limitation of the upstream server, not of the
gateway.

## Routing between INSEE components

- **Publication or written analysis**: search `insee` with
  `insee_search_insee_documents`, then read the result with `insee_get_insee_document`.
  For a single figure, prefer `insee_search_insee_chiffrecle`; for a recent indicator or a
  generic question about indicators, use `insee_get_insee_homepage` or
  `insee_search_insee_conjoncture` as needed.
- **Granular historical series or observations**: switch to `melodi`, starting with
  `melodi_search_melodi_datasets`, then `melodi_search_melodi_modalities` and
  `melodi_get_melodi_observations`. Dataset search is lexical: write an explicit query in
  French. Never guess an opaque identifier or a modality code: reuse the values returned by
  the search tools.
- **Definition, ontology, vocabulary or classification**: switch to `rmes`. Explore with
  `rmes_search_rmes_graphs`, query with `rmes_run_rmes_sparql` and, for the details of a
  resource, use `rmes_describe_rmes_resource`. RMES answers with metadata only, not with
  figures or observations.

In particular, publication descriptions point to RMES for definitions, codes and
classifications, and to MELODI for fine-grained historical series. Key figures do not
replace MELODI's granular observations. Conversely, MELODI is not the right entry point for
an up-to-date generic indicator, the full-text analysis of a published report, or a
definition or ontology: use the insee.fr or RMES tools listed above respectively. If a
MELODI observation needs a concept explanation or a code vocabulary, complement it with
RMES.

## Workflow 1 — Profile of an area

The key point: `geodata` works with coordinates (`lon`/`lat`), INSEE statistics with area
names (`geo_keyword` + `geo_level`). Geocoding is the entry point:

1. `geodata_geocode(text="Beauvais")` → returns a label, a type and `lon`/`lat`
   coordinates reusable in the geographic tools.
2. `insee_search_insee_chiffrecle(query="population, logement, emploi",
   geo_level="COM", geo_keyword="Beauvais")` → ready-to-use summary tables.
   `geo_level` accepts `COM`, `DEP`, `REG`, `INTER`, `COMPRD`, `FRANCE`.
3. For the geographic context at the same point: `geodata_adminexpress(lat=…, lon=…)`
   (administrative units), `geodata_cadastre(lat=…, lon=…)`, `geodata_urbanisme(lat=…, lon=…)`
   and `geodata_altitude(lat=…, lon=…)`.

Every response is data; composing the summary is up to you.

## Workflow 2 — Find a publication, then read it

1. `insee_search_insee_documents(query="effets du covid sur le tourisme",
   geo_level="REG", geo_keyword="Occitanie", year_of_reference=2024)` — write the query in
   natural language with synonyms and context (year, geography).
2. `insee_get_insee_document(document_urls=["<result url>"])` → full text.
3. For a single figure rather than a written analysis, use
   `insee_search_insee_chiffrecle`. For a recent monthly indicator, use
   `insee_search_insee_conjoncture`.

## Workflow 3 — Explore a statistical series (MELODI)

Go through MELODI in this order:

1. `melodi_search_melodi_datasets(query="indice des prix à la consommation",
   start_year=2015)` → candidate datasets and their identifiers. Write the query in
   explicit French (for example « décès par département » or « prénoms des
   nouveau-nés »).
2. `melodi_search_melodi_modalities(dataset_id=…, column_ids=["GEO"], query="…")`
   → valid modality codes. Reuse the returned codes; do not invent filters.
3. `melodi_get_melodi_observations(dataset_id=…, years=[2020, 2021],
   column_filters={"GEO": "<modality code>"}, number_of_observations=…)` → observations,
   dimensions, units and measures.

To explain a concept or a code used by the series, complement the search with the `rmes`
tools described above; RMES provides meaning and metadata, not values.

## Workflow 4 — One question, all three components

Question: "How have consumer prices changed, which datasets can verify it, and what does
the nomenclature used mean?" Follow the publications → data → metadata path and pass the
identifiers returned to the next step:

1. `insee_search_insee_documents(query="évolution des prix à la consommation",
   year_of_reference=2024)` → identify a relevant publication or analysis. If needed, read
   it with `insee_get_insee_document(document_urls=["<result url>"])`.
2. `melodi_search_melodi_datasets(query="indice des prix à la consommation",
   start_year=2015)` → identify a dataset; take its `dataset_id` from the result.
   Then look up the required codes with
   `melodi_search_melodi_modalities(dataset_id=…, column_ids=["GEO"], query="France")`,
   and read the relevant observations with `melodi_get_melodi_observations`, reusing the
   returned identifier and codes.
3. `rmes_search_rmes_graphs(graph_category="produits")` → identify the relevant metadata
   graph or resource (choose `nomenclatures` for a nomenclature). Use
   `rmes_run_rmes_sparql` with a query filtered on the discovered graph(s), then
   `rmes_describe_rmes_resource` if the details of an identified resource are needed.

The three answers are complementary: the publication gives the editorial context, MELODI
the numeric series and RMES the meaning of concepts and codes. Search results may be empty;
in that case, rephrase or switch component as needed, without fabricating an identifier, a
code or a result.

## Workflow 5 — Géoplateforme WFS data

1. `geodata_gpf_wfs_search_types(query="…")` → available types.
2. `geodata_gpf_wfs_describe_type(typename=…)` → schema (identifiers and attributes).
3. `geodata_gpf_wfs_get_features(typename=…, spatial_operator="bbox",
   bbox_west=…, bbox_south=…, bbox_east=…, bbox_north=…)` or
   `geodata_gpf_wfs_get_feature_by_id(typename=…, feature_id=…)`. For an attribute filter,
   fill in `where` with a property found in the describe step.

No need to write CQL: attribute and spatial filters are parameters.

## Operational reminders

- Discovery: `catalog://servers`, `catalog://domains`, then
  `catalog://servers/{name}/tools`, or directly the inventories documented here:
  `catalog://servers/insee/tools`, `catalog://servers/melodi/tools`,
  `catalog://servers/rmes/tools`, `catalog://servers/mcpdiffusion/tools` and
  `catalog://servers/geodata/tools`. Tool search (if enabled) covers the text of the
  tools — "melodi", "cadastre", "insee" work, a domain word like "diffusion" does not.
- An upstream may be temporarily unavailable: `catalog://servers` carries its status;
  `/health` (HTTP) gives the detailed status and metrics.
- Operator feedback: call `send_feedback(server="insee", author="…",
  feedback="…")` to report a problem with a component or a server. The name must be that
  of a registry server; the report is recorded in the gateway log, without depending on an
  upstream service.
