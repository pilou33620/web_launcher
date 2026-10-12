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
3. crée le service systemd `web_launcher` : il démarre à l'allumage, sur le **port 80**
   (sans être root), et redémarre tout seul en cas de problème ;
4. propose de connecter le Pi à GitHub si `gh` est installé (dépôts privés, voir plus bas).

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
- **Mises à jour** : le launcher regarde GitHub toutes les 6 heures (et avec le bouton
  *Vérifier les mises à jour*). **Mettre à jour** marche même si l'outil tourne : arrêt,
  `git pull --ff-only`, réinstallation des bibliothèques si `requirements.txt` a changé,
  redémarrage. Jamais de fusion ni d'écrasement : si des fichiers suivis par git ont été
  modifiés sur le Pi, la mise à jour est refusée et les fichiers sont nommés.
- Les mises à jour automatiques propres à chaque outil sont coupées (`--maj 0`,
  `--no-update`, `--sans-maj`) : le launcher est le seul à mettre à jour, et prévient.
- Le **launcher lui-même** se met à jour depuis le pied de page. Il redémarre en quelques
  secondes ; les outils continuent de tourner et sont retrouvés.

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
effacement, Ctrl+C, Ctrl+D ; pas de Ctrl+Z ni de séquences d'échappement). Mais tout
appareil du réseau local peut lire ce que les outils affichent et répondre à leurs
questions. Le jeton réseau de WEB·SUITE est masqué (`jeton=•••`). Pour un outil sans
terminal : `"terminal": false` dans `config.local.json`.

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

## Réglages propres à ce Pi

`outils.json` (suivi par git) décrit les outils. Pour changer un réglage sans le modifier,
créer `config.local.json` (ignoré par git) avec seulement ce qui change :

```json
{
  "port": 80,
  "dossier_outils": "~/Documents",
  "verification_maj_heures": 6,
  "outils": [
    { "id": "web_menu", "port": 8000 },
    { "id": "web_conquete", "masque": true }
  ]
}
```

- un `id` existant : remplace ces champs (port, commande...) ; `"masque": true` cache l'outil ;
- un nouvel `id` : ajoute un outil (`nom`, `depot`, `dossiers`, `port`, `commande` avec
  `{python}` et `{port}`, `venv`, `accent`...). Voir `outils.json` pour des exemples ;
- `"nom_mdns": "launcher"` : le nom annoncé (`launcher.local`), `null` pour ne rien annoncer ;
- `"verification_maj_heures": 0` : ne jamais regarder GitHub tout seul.

Puis `sudo systemctl restart web_launcher`.

## Fichiers

```
launcher.py              point d'entrée
lanceur/serveur.py       serveur web et API (/api/etat, /api/outils/<id>/<action>, /api/journal/<id>, /api/console/<id>)
lanceur/gestionnaire.py  démarrage, arrêt, reprise, installation, mises à jour
lanceur/depot.py         git (fetch, pull --ff-only, clone)
lanceur/reseau.py        IP, ports, annonce mDNS de launcher.local
lanceur/console.py       terminal d'un outil (pseudo-terminal, saisie via journaux/<id>.entree)
web/                     les pages (launcher, Terminaux), HTML, CSS, JS sans dépendance
outils.json              catalogue des outils
installer.sh             installation du service sur le Pi
journaux/<id>.log        sortie de chaque outil (créé tout seul, ignoré par git)
etat.local.json          outils allumés et leurs PID (créé tout seul, ignoré par git)
```

Tests : `python3 -m unittest discover tests` (un faux outil et un dépôt git local,
sans réseau).

## Sécurité

Pas de mot de passe : le launcher est prévu pour le réseau de la maison. Tout appareil
du réseau local peut démarrer, arrêter et mettre à jour les outils. Les autres sites
web ne le peuvent pas (en-tête `X-Launcher` exigé, nom d'hôte contrôlé contre le DNS
rebinding). **Ne pas exposer le port 80 du Pi sur Internet** (pas de redirection de
port sur la box).

## Dépannage

- *« Le port 8080 est déjà pris par un autre programme »* : un outil tourne hors du
  launcher sous un autre utilisateur, ou un autre programme utilise ce port. Changer le port
  dans `config.local.json`.
- *« Lancé hors launcher »* : l'outil a été démarré à la main ou par son ancien service. Si
  c'est le même utilisateur, le launcher le reprend tout seul (bouton Arrêter compris).
- `launcher.local` ne répond pas : `systemctl status avahi-daemon`, puis
  `journalctl -u web_launcher` (« avahi-publish introuvable » → `sudo apt install avahi-utils`).
- Le journal de chaque outil : menu **…** → *Voir le journal*, ou `journaux/<id>.log`.
