# web_launcher

Une page web, toujours allumée sur le Raspberry Pi, pour piloter tous les web tools :
**démarrer / arrêter**, **ouvrir** dans un nouvel onglet, voir leur **adresse locale**,
**vérifier et installer les mises à jour** GitHub (l'outil redémarre tout seul), et
**télécharger** un outil absent de `~/Documents`.

Depuis n'importe quel appareil du réseau local : **http://launcher.local**

| Outil | Dépôt | Port | Dossier |
|---|---|---|---|
| ToothDentaire | [tooth_dentaire](https://github.com/pilou33620/tooth_dentaire) | 8150 | `~/Documents/tooth_dentaire` |
| MenuGamelles | [web_menu](https://github.com/pilou33620/web_menu) (privé) | 8080 | `~/Documents/web_menu` |
| Conquête1790 | [web_conquete](https://github.com/pilou33620/web_conquete) | 8790 | `~/Documents/web_conquete` |
| WebSuite | [web_suite](https://github.com/pilou33620/web_suite) | 8100 | `~/Documents/web_suite` (ou `WEB_TOOLS`, `WEB_SUITE`) |

Python 3.8+ et git, bibliothèque standard uniquement : rien à installer avec pip.

## Installation sur le Raspberry Pi

```bash
cd ~/Documents
git clone https://github.com/pilou33620/web_launcher.git
cd web_launcher
./installer.sh
```

L'installateur :

1. installe ce qui manque : `git`, `python3-venv` (pour tooth_dentaire), `avahi-utils` (pour `launcher.local`) ;
2. propose de désactiver les anciens services `gamelles` (web_menu) et `web_conquete`
   s'ils existent : c'est le launcher qui gère ces outils désormais ;
3. demande l'**identifiant et le mot de passe** de la page web (voir « Compte ») ;
4. crée le service systemd `web_launcher` : il démarre à l'allumage, sur le **port 80**
   (sans être root), et redémarre tout seul en cas de problème ;
5. propose de connecter le Pi à GitHub si `gh` est installé (dépôts privés, voir plus bas).

À la fin, il affiche les adresses du launcher. Elles sont aussi en haut de la page.

```bash
systemctl status web_launcher        # état
journalctl -u web_launcher -f        # journal du launcher
./installer.sh --retirer             # supprimer le service
```

Sans le service (essai, autre machine) : `python3 launcher.py --port 8090`.

## Les adresses

- **http://launcher.local** : le launcher annonce ce nom sur le réseau local (mDNS, via
  `avahi-publish`), en plus du nom normal du Pi. Le port 80 permet de ne pas taper de numéro.
  Fonctionne sur iPhone, iPad, Mac, Windows 10/11 et Linux. Sur Android, selon le navigateur
  et la version, les noms en `.local` ne sont pas toujours reconnus : utiliser l'adresse IP.
- **http://192.168.x.x** : l'adresse IP du Pi, affichée en haut de la page (bouton *copier*).
- **http://<nom-du-pi>.local** : le nom habituel du Pi (`raspberrypi.local` par défaut).

Chaque carte affiche l'adresse de son outil (`http://<IP du Pi>:<port>/`) et un bouton
**Ouvrir ↗**. L'IP est utilisée plutôt que `launcher.local` parce que certains outils
(tooth_dentaire, WEB·SUITE) refusent par sécurité un nom qu'ils ne connaissent pas.
Ouvert sur le Pi lui-même, le launcher envoie vers `127.0.0.1`.

Si l'IP du Pi change (box redémarrée), `launcher.local` suit tout seul. Pour une adresse
stable, réserver l'IP du Pi dans la box (bail DHCP fixe).

## Utilisation

Une carte par outil :

| État | Boutons |
|---|---|
| Disponible à l'installation | **Installer** : `git clone` dans `~/Documents`, puis la suite de l'installation |
| Arrêté | **Démarrer** |
| En marche | **Ouvrir ↗**, **Arrêter** |
| Mise à jour disponible | **Mettre à jour** (avec la liste des nouveautés) |

Le menu **…** : redémarrer, vérifier la mise à jour, mettre à jour maintenant, ouvrir son
**terminal**, voir le **journal**, copier l'adresse.

- **Toujours allumé** : un outil allumé reste allumé après un redémarrage du Pi (badge
  *Auto*). **Arrêter** le retire. S'il plante, il est relancé (3 fois en 10 minutes au plus,
  ensuite l'erreur reste affichée avec le journal).
- **Mises à jour**, à la main ou automatiques :
  - **à la main** : **Vérifier les mises à jour** (en haut, ou menu **…** d'un outil)
    regarde GitHub et affiche les nouveautés ; **Mettre à jour** les installe ;
  - **vérification automatique à chaque démarrage du launcher** (30 s après l'allumage du
    Pi, quelques secondes après un redémarrage du launcher), **puis toutes les 6 heures** :
    les nouveautés s'affichent sur les cartes, rien n'est installé ; l'heure de la
    prochaine vérification est en bas de la page ;
  - **Mise à jour auto** (menu **…**, outil par outil, désactivée par défaut) : à chaque
    vérification automatique, la nouvelle version est installée et l'outil redémarre
    (quelques secondes d'interruption). Badge *MAJ auto* sur la carte.

  La mise à jour marche même si l'outil tourne : arrêt,
  `git pull --ff-only`, réinstallation des bibliothèques si `requirements.txt` a changé,
  redémarrage. Jamais de fusion ni d'écrasement : si des fichiers suivis par git ont été
  modifiés sur le Pi, la mise à jour est refusée et les fichiers sont nommés.
- Les mises à jour automatiques propres à chaque outil sont coupées (`--maj 0`,
  `--no-update`, `--sans-maj`) : le launcher est le seul à mettre à jour, et prévient.
- Le **launcher lui-même** se met à jour depuis le pied de page. Il redémarre en quelques
  secondes ; les outils continuent de tourner et sont retrouvés.
- **Redémarrer le launcher** (pied de page) : relance le serveur avec le code présent sur le
  Pi, sans mot de passe ni terminal (comme `sudo systemctl restart web_launcher`). Les
  outils continuent de tourner ; les mises à jour sont vérifiées dans la foulée.

## Terminaux

**Terminaux ↗** (en haut de la page, ou menu **…** → *Terminal*) ouvre une fenêtre à part
avec un onglet par outil : tout ce que l'outil affiche, en direct, comme dans une fenêtre de
terminal, et une ligne pour lui **répondre** (Entrée pour envoyer) et un bouton **Ctrl+C**.

Chaque outil tourne dans un vrai terminal (pseudo-terminal tenu par `lanceur/console.py`) :
il peut donc poser ses questions au démarrage. Quand une question attend une réponse,
la carte affiche **Attend une réponse ↗** et la fenêtre Terminaux le signale. Exemple :
WEB·SUITE, tant que le Pi n'est pas connecté à GitHub, demande à chaque démarrage
« Se connecter maintenant ? [O/n, j = ne plus demander] » : répondre `o` pour se connecter
(le code `gh` s'affiche dans le terminal), `n` pour plus tard, `j` pour ne plus être
interrogé. Tant que la question est sans réponse, WEB·SUITE ne démarre pas.

Le terminal survit aux redémarrages du launcher. Un Ctrl+C arrête la plupart des outils :
l'arrêt est alors considéré comme voulu (pas de relance automatique).

**Sécurité** : ce n'est **pas** un shell. On ne peut ni lancer de commande sur le Pi ni
ouvrir bash : seulement écrire dans l'entrée de l'outil déjà lancé (texte, Entrée, Tab,
effacement, Ctrl+C, Ctrl+D ; pas de Ctrl+Z ni de séquences d'échappement). Seul un
appareil connecté au launcher peut lire ce que les outils affichent et répondre à leurs
questions (voir « Compte »). Le jeton réseau de WEB·SUITE est masqué (`jeton=•••`).
Pour un outil sans terminal : `"terminal": false` dans `config.local.json`.

## Particularités des outils

**tooth_dentaire** : installé avec un environnement Python `.venv` (`openpyxl` pour les
exports Excel, `certifi` pour la météo). La base reste dans `donnees/`, jamais touchée.

**web_menu** : dépôt privé (voir plus bas). `profils.local.js` et `donnees.local.json`
restent sur le Pi, jamais touchés par les mises à jour.

**web_conquete** : lancé avec `--lan`, sauvegardes dans `saves/`.

**web_suite** : c'est lui-même un lanceur pour WEB_CAO, WEB_ANTENNA et WEB_3D.

- *Installer* clone web_suite puis lance son installateur (`lanceur/installer.py tous`),
  qui télécharge les trois outils dans le dossier de web_suite.
- *Démarrer* lance son lanceur en mode réseau, port 8100. WEB_CAO, WEB_3D et WEB_ANTENNA se
  lancent ensuite depuis la page de WEB·SUITE (bouton *Ouvrir ↗*).
- *Mettre à jour* met à jour web_suite **et** WEB_CAO, WEB_ANTENNA, WEB_3D (la carte compte
  les nouveautés des quatre dépôts), puis redémarre. Le dossier `PROJETS/` n'est pas touché
  par le launcher : WEB·SUITE le synchronise lui-même.
- **Code d'appairage** : WEB·SUITE demande un code à 6 chiffres à chaque nouvel appareil.
  Normalement affiché dans son terminal, il est ici affiché sur sa carte (valable 10 min).
  Conséquence : toute personne qui ouvre le launcher peut appairer un appareil. Le jeton
  réseau, lui, est masqué dans le journal.
- WEB_ANTENNA a besoin d'openEMS pour simuler (pas installé automatiquement sur Pi, voir
  le README de web_suite).
- Le dépôt des projets (`WEB_SUITE_PROJETS`) est privé : connecter le Pi à GitHub.

## Dépôts privés

`web_menu` et les projets WEB·SUITE sont privés : le Pi doit pouvoir faire `git clone` /
`git pull` **sans rien taper** (le launcher ne peut pas demander de mot de passe). Une fois :

```bash
sudo apt install gh
gh auth login          # GitHub.com, HTTPS, connexion par le navigateur (code à saisir)
gh auth setup-git      # git utilise ce compte pour cloner et mettre à jour
```

Sans connexion, la carte affiche « GitHub refuse l'accès (dépôt privé ?) ».

## Réglages propres à ce Pi (facultatif)

**Rien à faire si tout vous convient** : le launcher fonctionne sans ce qui suit.

**Depuis la page** : bouton **Réglages** (en haut). Pour chaque outil, son **port** et
**Afficher** (décoché : l'outil disparaît de la page, il revient en recochant). L'outil
doit être arrêté. C'est enregistré dans `config.local.json` (voir ci-dessous) et appliqué
tout de suite, sans redémarrer le launcher. Le panneau gère aussi les **comptes**.

**À la main**, pour les autres réglages :

Les réglages par défaut sont dans `outils.json` : la liste des outils (dépôt GitHub, port,
commande de lancement), le port du launcher (80), le dossier `~/Documents`, la vérification
GitHub toutes les 6 h. **Ne pas modifier ce fichier sur le Pi** : il est suivi par git, et
une fois modifié, il bloquerait les mises à jour du launcher.

Pour changer un réglage, créer à côté un fichier `config.local.json` (jamais envoyé sur
GitHub, jamais touché par les mises à jour) avec **seulement ce qui change**. Exemples :

| Besoin | Contenu de `config.local.json` |
|---|---|
| Un port est déjà pris : web_menu sur 8000 au lieu de 8080 | `{ "outils": [ { "id": "web_menu", "port": 8000 } ] }` |
| Cacher un outil qu'on n'utilise pas | `{ "outils": [ { "id": "web_conquete", "masque": true } ] }` |
| Vérifier GitHub toutes les 12 h au lieu de 6 | `{ "verification_maj_heures": 12 }` |
| Ne vérifier GitHub qu'au démarrage du launcher et sur clic (la *Mise à jour auto* n'agit alors qu'au démarrage) | `{ "verification_maj_heures": 0 }` |
| Un autre nom que `launcher.local` (ici `maison.local`) | `{ "nom_mdns": "maison" }` |
| Ne pas annoncer de nom `.local` | `{ "nom_mdns": null }` |

Plusieurs réglages à la fois, dans un seul fichier :

```json
{
  "verification_maj_heures": 12,
  "outils": [
    { "id": "web_menu", "port": 8000 },
    { "id": "web_conquete", "masque": true }
  ]
}
```

Ajouter un nouveau web tool : une entrée avec un nouvel `id` dans `"outils"` (`nom`,
`depot`, `dossiers`, `port`, `commande` avec `{python}` et `{port}`...), sur le modèle de
ceux de `outils.json`.

Ensuite, redémarrer le launcher pour qu'il relise le fichier (les outils continuent de
tourner) :

```bash
sudo systemctl restart web_launcher
```

## Fichiers

```
launcher.py              point d'entrée
lanceur/serveur.py       serveur web et API (/api/etat, /api/outils/<id>/<action>, /api/journal/<id>, /api/console/<id>)
lanceur/gestionnaire.py  démarrage, arrêt, reprise, installation, mises à jour
lanceur/depot.py         git (fetch, pull --ff-only, clone)
lanceur/reseau.py        IP, ports, annonce mDNS de launcher.local
lanceur/compte.py        comptes, mots de passe hachés, sessions
lanceur/console.py       terminal d'un outil (pseudo-terminal, saisie via journaux/<id>.entree)
web/                     les pages (launcher, Terminaux), HTML, CSS, JS sans dépendance
outils.json              catalogue des outils
installer.sh             installation du service sur le Pi
journaux/<id>.log        sortie de chaque outil (créé tout seul, ignoré par git)
etat.local.json          outils allumés et leurs PID (créé tout seul, ignoré par git)
```

Tests : `python3 -m unittest discover tests` (un faux outil et un dépôt git local,
sans réseau).

## Comptes

La page du launcher (cartes, terminaux, journaux, API) demande un **identifiant et un mot
de passe**. Il peut y avoir **plusieurs comptes**, un par personne : ils ont tous les mêmes
droits. Le premier est créé par `installer.sh`.

- **Depuis la page**, **Réglages** → *Comptes* : ajouter un compte, changer le mot de passe
  d'un compte, supprimer un compte (sauf le dernier).
- **Sur le Pi** (en SSH), par exemple en cas d'oubli :

```bash
python3 launcher.py --compte     # identifiant nouveau : ajouté ; existant : nouveau mot de passe
```

- Le mot de passe n'est **jamais stocké en clair** : seule son empreinte **scrypt** (sel
  aléatoire, calcul volontairement lent et gourmand en mémoire) est dans
  `compte.local.json`, lisible par son seul propriétaire (`chmod 600`), ignoré par git.
  Un vol de ce fichier ne donne pas le mot de passe.
- Connecté, l'appareil reçoit un jeton de session aléatoire (256 bits) dans un cookie
  `HttpOnly` / `SameSite=Strict`, valable 30 jours. Le launcher n'en garde que l'empreinte
  SHA-256 (`sessions.local.json`). **Déconnexion** en haut de la page.
- 5 erreurs depuis une même adresse la bloquent 5 minutes.
- Changer le mot de passe d'un compte déconnecte ses autres appareils ; supprimer un compte
  déconnecte tous ses appareils. Les autres comptes ne sont pas touchés.
- Mot de passe oublié : `python3 launcher.py --compte` sur le Pi (en SSH).
- Sans compte créé, la page reste ouverte à tous et l'affiche en rouge.

## Sécurité

- **Ce que protège le compte** : le launcher (démarrer, arrêter, mettre à jour, installer,
  terminaux, journaux). Les autres sites web ne peuvent pas non plus le piloter (cookie
  `SameSite=Strict`, en-tête `X-Launcher` exigé, nom d'hôte contrôlé contre le DNS rebinding).
- **Ce qu'il ne protège pas** : les outils eux-mêmes, sur leurs ports (8150, 8080...),
  restent ouverts au réseau local comme avant. WEB·SUITE garde son propre code d'appairage.
- **Connexion en HTTP** : sur le réseau de la maison, la page n'est pas chiffrée (pas de
  HTTPS, qui demanderait un certificat et afficherait des alertes dans les navigateurs).
  Le mot de passe est protégé là où il est stocké, mais il passe en clair sur le Wi-Fi au
  moment de la connexion : choisir un mot de passe propre au launcher, pas celui de la
  messagerie.
- **Ne pas exposer le port 80 du Pi sur Internet** (pas de redirection de port sur la box).

## Dépannage

- *« Le port 8080 est déjà pris par un autre programme »* : un outil tourne hors du
  launcher sous un autre utilisateur, ou un autre programme utilise ce port. Changer le port
  dans `config.local.json`.
- *« Lancé hors launcher »* : l'outil a été démarré à la main ou par son ancien service. Si
  c'est le même utilisateur, le launcher le reprend tout seul (bouton Arrêter compris).
- `launcher.local` ne répond pas : `systemctl status avahi-daemon`, puis
  `journalctl -u web_launcher` (« avahi-publish introuvable » → `sudo apt install avahi-utils`).
- Le journal de chaque outil : menu **…** → *Voir le journal*, ou `journaux/<id>.log`.
