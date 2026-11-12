# Utiliser la passerelle MCP

Cette passerelle agrège plusieurs serveurs MCP derrière une seule connexion. Ce guide décrit
sa mécanique ; elle est la même quel que soit le déploiement.

## Nommage des outils

Chaque outil d'un serveur amont est exposé sous le nom `{serveur}_{outil}` : le nom du
serveur sert de préfixe. C'est toujours ce nom complet qu'il faut utiliser pour appeler un
outil, jamais le nom amont seul.

## Trouver puis appeler un outil

Quand la recherche d'outils est activée, la liste des outils (`tools/list`) ne montre pas les
outils des serveurs amont, seulement trois outils de la passerelle :

1. `search_tools(query)` : recherche plein texte (mots-clés, en français de préférence) dans
   les noms, descriptions et paramètres de tous les outils. Chaque résultat donne le nom à
   appeler et son `inputSchema` complet.
2. `call_tool(name, arguments)` : appelle l'outil `name` (nom complet `{serveur}_{outil}`).
   `arguments` est un **objet JSON** conforme à l'`inputSchema` renvoyé par la recherche,
   pas une chaîne de caractères.
3. `send_feedback(server, author, feedback)` : signale un problème ou une remarque sur un
   serveur. Le message est consigné dans le journal de la passerelle.

Démarche type : `search_tools` → choisir un résultat → `call_tool` avec ses arguments.

Quand la recherche d'outils est désactivée, les outils amont apparaissent directement dans
`tools/list`, sous leur nom complet, et s'appellent normalement.

## Le catalogue (ressources)

Le catalogue est exposé en ressources : le lire ne coûte rien tant qu'on ne le demande pas.

| Ressource | Contenu |
| --- | --- |
| `catalog://servers` | tous les serveurs : domaine, description, tags, état, limites |
| `catalog://domains` | serveurs regroupés par domaine fonctionnel |
| `catalog://servers/{nom}` | détail d'un serveur et ses métriques (outils, sondes) |
| `catalog://servers/{nom}/tools` | inventaire : nom amont, nom à appeler, description, paramètres |
| `catalog://guide` | ce guide, suivi des démarches propres au déploiement |

Un serveur injoignable ne casse pas le catalogue : sa ressource répond
`"status": "unavailable"` avec l'erreur.

## Pièges courants

- **Ne devinez pas un nom d'outil** : cherchez-le avec `search_tools` ou lisez l'inventaire.
  Un nom inconnu renvoie une erreur qui propose les noms proches.
- **Arguments invalides** : la passerelle valide les arguments contre le schéma de l'outil
  avant de l'appeler et renvoie le détail des erreurs. Corrigez et réessayez.
- **Résultat en erreur** (`isError: true`) : l'appel a abouti mais l'outil a échoué ; le
  message explique pourquoi. Ce n'est pas une panne de la passerelle.
- **Passerelle ou serveur saturé** : le nombre d'appels simultanés est limité. Réessayez
  quelques secondes plus tard plutôt que de multiplier les appels en parallèle.
- **Serveur indisponible** : consultez `catalog://servers` pour son état ; utilisez un autre
  serveur ou réessayez plus tard.
- **Recherche vide** : reformulez avec d'autres mots-clés ou des synonymes ; la recherche
  porte sur le texte des outils, pas sur les données qu'ils renvoient.
