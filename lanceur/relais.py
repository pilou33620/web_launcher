"""Relais : ouvre un web tool à travers le launcher, à l'adresse /o/<id>/.

Pourquoi : depuis Internet (Tailscale Funnel), une seule adresse https est joignable, celle
du launcher. Les outils, eux, écoutent sur leurs propres ports (8150, 8080...), inaccessibles
de l'extérieur. Le launcher fait donc passer leurs pages par lui, après sa connexion.

- /o/<id>/...  : relayé vers l'outil, sur 127.0.0.1:<port>, préfixe retiré.
- Un outil écrit souvent des liens absolus ("/api/x", "/css/a.css") : ils sont réécrits dans
  le HTML, et ceux que le HTML ne contient pas (fetch, CSS) sont retrouvés grâce au Referer
  (la page d'où part la requête est sous /o/<id>/).
- WebSocket : retrouvé par le cookie `relais_outil` (la poignée de main n'envoie pas de Referer),
  puis tunnel brut dans les deux sens.
- Le cookie de session du launcher n'est jamais transmis à l'outil, ni l'en-tête Authorization.
- Les outils partagent l'adresse du launcher : un outil ne peut ni poser les cookies du launcher,
  ni installer un service worker (il pourrait sinon prendre la main sur toute l'adresse).
- Envoi (POST, PUT...) et WebSocket : l'Origin doit être l'adresse du launcher (un autre site,
  ou un outil ouvert sur son propre port, ne passe pas par le relais avec la session).
"""

import http.client
import re
import select
import socket
from http.cookies import CookieError, SimpleCookie
from urllib.parse import urlparse

PREFIXE = "/o/"
COOKIE_OUTIL = "relais_outil"
ID = r"[A-Za-z0-9_-]+"
DANS_PREFIXE = re.compile(r"^/o/(%s)(/.*)?$" % ID)
HTML_MAX = 5 * 1024 * 1024
ATTENTE_TUNNEL = 600

# En-têtes qui ne se transmettent pas tels quels (propres à la connexion, ou réécrits ici).
SAUF_REQUETE = {"host", "connection", "keep-alive", "proxy-authorization", "proxy-connection", "te", "trailer",
                "transfer-encoding", "upgrade", "content-length", "accept-encoding", "authorization",
                "cookie", "origin", "referer", "expect", "tailscale-user-login", "tailscale-user-name",
                "tailscale-user-profile-pic", "tailscale-funnel-request", "tailscale-headers-info"}
SAUF_REPONSE = {"connection", "keep-alive", "proxy-authenticate", "proxy-connection", "te", "trailer",
                "transfer-encoding", "upgrade", "content-length", "content-encoding", "referrer-policy",
                "service-worker-allowed", "clear-site-data", "strict-transport-security"}
COOKIES_LAUNCHER = ("launcher_session", COOKIE_OUTIL)
SANS_EFFET = ("GET", "HEAD", "OPTIONS")

LIENS_HTML = re.compile(rb"""(\b(?:href|src|action|formaction|poster)\s*=\s*)(["'])/(?!/)""", re.I)


def prefixe(id_):
    return "%s%s/" % (PREFIXE, id_)


def _cookies(h):
    try:
        return SimpleCookie(h.headers.get("Cookie") or "")
    except CookieError:
        return SimpleCookie()


def _hote(h):
    return h.hote_public() if hasattr(h, "hote_public") else (h.headers.get("Host") or "").strip().lower()


def _id_du_referer(h):
    """L'outil d'où part la requête, d'après le chemin du Referer (/o/<id>/...), ou None.
    Le nom d'hôte n'est pas comparé : derrière Funnel et Caddy, celui que voit le launcher varie,
    et ce serveur ne sert que des outils (la session, cookie Strict, reste exigée)."""
    m = DANS_PREFIXE.match(urlparse(h.headers.get("Referer") or "").path)
    return m.group(1) if m else None


def cible(h):
    """(id, chemin pour l'outil) si cette requête va à un outil, sinon None."""
    chemin = h.path
    if not chemin.startswith("/"):
        return None
    m = DANS_PREFIXE.match(urlparse(chemin).path)
    if m:
        reste = chemin[len(PREFIXE) + len(m.group(1)):]      # "" (sans barre finale), "/..." ou "?..."
        return m.group(1), ("/" + reste) if reste.startswith("?") else reste
    if chemin.startswith(PREFIXE):
        return None
    id_ = _id_du_referer(h)
    if id_ is None:
        # sans Referer (WebSocket, fetch « no-referrer ») : le dernier outil ouvert dans ce navigateur
        biscuit = _cookies(h).get(COOKIE_OUTIL)
        id_ = biscuit.value if biscuit and re.fullmatch(ID, biscuit.value) else None
    return (id_, chemin) if id_ else None


def _chemin_outil(chemin):
    return "/" + chemin.lstrip("/") if not chemin.startswith("/") or chemin.startswith("//") else chemin


def _entetes(h, hote, tunnel):
    sortie = []
    for k, v in h.headers.items():
        bas = k.lower()
        if bas in SAUF_REQUETE and not (tunnel and bas in ("connection", "upgrade")):
            continue
        sortie.append((k, v))
    sortie.append(("Host", hote))
    origine = h.headers.get("Origin")
    if origine:
        sortie.append(("Origin", "http://" + hote if origine != "null" else origine))
    ref = h.headers.get("Referer")
    if ref:
        p = urlparse(ref)
        m = DANS_PREFIXE.match(p.path)
        chemin = (m.group(2) or "/") if m else p.path or "/"
        sortie.append(("Referer", "http://%s%s%s" % (hote, chemin, "?" + p.query if p.query else "")))
    # le cookie du launcher reste au launcher
    biscuits = [c.OutputString() for nom, c in _cookies(h).items() if nom not in ("launcher_session", COOKIE_OUTIL)]
    if biscuits:
        sortie.append(("Cookie", "; ".join(biscuits)))
    if not tunnel:
        sortie.append(("Accept-Encoding", "identity"))
    return sortie


def _location(valeur, id_, hote):
    """Redirection de l'outil : ses adresses absolues repassent par le relais."""
    p = urlparse(valeur)
    if p.netloc and p.netloc.lower() != hote:
        return valeur                                   # un autre site : inchangé
    if not p.netloc and not valeur.startswith("/"):
        return valeur                                   # relative : le navigateur la résout
    chemin = _chemin_outil(p.path or "/") + ("?" + p.query if p.query else "") + ("#" + p.fragment if p.fragment else "")
    return prefixe(id_)[:-1] + chemin


def _biscuit(valeur):
    """Set-Cookie de l'outil : sans Domain (il serait celui du Pi, pas du site visité).
    None : cookie du launcher, refusé."""
    nom = valeur.split("=", 1)[0].strip().lower()
    if nom in COOKIES_LAUNCHER or nom.startswith(("__host-", "__secure-")) and nom.split("-", 1)[1] in COOKIES_LAUNCHER:
        return None
    return re.sub(r";\s*Domain=[^;]*", "", valeur, flags=re.I)


def origine_permise(h):
    """Requête qui agit (envoi, WebSocket) : elle doit partir d'une page de cette adresse.
    Sec-Fetch-Site est posé par le navigateur lui-même (une page ne peut pas le changer) ;
    à défaut (vieux navigateur), l'Origin est comparée au nom d'hôte."""
    site = (h.headers.get("Sec-Fetch-Site") or "").strip().lower()
    if site:
        return site in ("same-origin", "none")
    origine = h.headers.get("Origin")
    if origine is None:
        return True                                     # pas un envoi depuis une page web
    return urlparse(origine).netloc.lower() == _hote(h) and origine != "null"


def relayer(h, port, id_, chemin):
    """Transmet la requête de h à l'outil et renvoie sa réponse. Jamais d'exception vers l'appelant."""
    hote = "127.0.0.1:%d" % port
    chemin = _chemin_outil(chemin)
    tunnel = (h.headers.get("Upgrade") or "").lower() == "websocket"
    if (tunnel or h.command not in SANS_EFFET) and not origine_permise(h):
        return h._texte("Requête refusée (autre site).", 403)
    if (h.headers.get("Service-Worker") or "").lower() == "script":
        return h._texte("Service worker refusé à travers le launcher.", 403)
    h.relais = True
    try:
        if tunnel:
            return _tunnel(h, port, hote, chemin)
        return _requete(h, port, hote, id_, chemin)
    except (OSError, http.client.HTTPException):
        h.close_connection = True
        if not getattr(h, "_entetes_envoyes", False):
            h.relais = False
            h._texte("L'outil ne répond pas (est-il démarré ?).", 502)


def _requete(h, port, hote, id_, chemin):
    methode = h.command
    longueur = None
    if "chunked" in (h.headers.get("Transfer-Encoding") or "").lower():
        h.relais = False
        return h._texte("Envoi en morceaux non pris en charge.", 411)
    if methode not in ("GET", "HEAD", "OPTIONS", "DELETE") or h.headers.get("Content-Length"):
        try:
            longueur = int(h.headers.get("Content-Length") or 0)
        except ValueError:
            h.relais = False
            return h._texte("Requête illisible.", 400)
        if longueur < 0:
            h.relais = False
            return h._texte("Requête illisible.", 400)
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=300)
    try:
        conn.putrequest(methode, chemin, skip_host=True, skip_accept_encoding=True)
        for k, v in _entetes(h, hote, False):
            conn.putheader(k, v)
        if longueur is not None:
            conn.putheader("Content-Length", str(longueur))
        conn.endheaders()
        restant = longueur or 0
        while restant:
            morceau = h.rfile.read(min(65536, restant))
            if not morceau:
                raise OSError("client coupé")
            conn.send(morceau)
            restant -= len(morceau)
        rep = conn.getresponse()
        type_ = rep.getheader("Content-Type") or ""
        taille = rep.getheader("Content-Length")
        html = type_.lower().startswith("text/html") and methode != "HEAD" and rep.status not in (204, 304) and \
            (taille is None or (taille.isdigit() and int(taille) <= HTML_MAX))
        corps = None
        if html:
            corps = rep.read(HTML_MAX + 1)
            if len(corps) > HTML_MAX:                   # trop gros pour être réécrit : on ne touche à rien
                h.relais = False
                return h._texte("Page trop grande pour le relais.", 502)
            racine = prefixe(id_)[:-1].encode() + b"/"
            corps = LIENS_HTML.sub(lambda m: m.group(1) + m.group(2) + racine, corps)
            taille = str(len(corps))
        h.send_response(rep.status, rep.reason)
        h._entetes_envoyes = True
        for k, v in rep.getheaders():
            bas = k.lower()
            if bas in SAUF_REPONSE:
                continue
            if bas == "location":
                v = _location(v, id_, hote)
            elif bas == "set-cookie":
                v = _biscuit(v)
                if v is None:
                    continue
            h.send_header(k, v)
        if taille is not None:
            h.send_header("Content-Length", taille)
        h.send_header("Referrer-Policy", "same-origin")
        options = h.options_cookie() if hasattr(h, "options_cookie") else "; Path=/; HttpOnly; SameSite=Strict"
        h.send_header("Set-Cookie", "%s=%s%s" % (COOKIE_OUTIL, id_, options))
        h.end_headers()
        if methode == "HEAD" or rep.status in (204, 304):
            return
        if corps is not None:
            h.wfile.write(corps)
            return
        while True:
            morceau = rep.read1(65536) if hasattr(rep, "read1") else rep.read(65536)
            if not morceau:
                break
            h.wfile.write(morceau)
            h.wfile.flush()
        if taille is None:
            h.close_connection = True                   # réponse sans longueur : fermée à la fin
    finally:
        conn.close()


def _tunnel(h, port, hote, chemin):
    """WebSocket : la demande part telle quelle (Host et Origin ramenés à l'outil), puis les octets
    passent dans les deux sens jusqu'à la fermeture."""
    arriere = socket.create_connection(("127.0.0.1", port), timeout=10)
    avant = h.connection
    try:
        demande = "%s %s HTTP/1.1\r\n" % (h.command, chemin)
        demande += "".join("%s: %s\r\n" % (k, v) for k, v in _entetes(h, hote, True)) + "\r\n"
        arriere.sendall(demande.encode("latin-1", "replace"))
        h._entetes_envoyes = True
        arriere.settimeout(None)
        avant.settimeout(None)
        paires = {avant: arriere, arriere: avant}
        while True:
            pret, _, _ = select.select(list(paires), [], [], ATTENTE_TUNNEL)
            if not pret:
                break
            fini = False
            for s in pret:
                donnees = s.recv(65536)
                if not donnees:
                    fini = True
                    break
                paires[s].sendall(donnees)
            if fini:
                break
    finally:
        arriere.close()
        h.close_connection = True
