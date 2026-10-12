"use strict";

const ICONES = {
  dent: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M7.5 3.5c-2.6 0-4 2-4 4.6 0 2.2.9 3.4 1.5 5 .6 1.7.7 4.4 1.3 6.1.4 1.1 1 1.6 1.7 1.6 1.4 0 1.6-2.6 2.1-4.4.3-1.1.8-1.7 1.9-1.7s1.6.6 1.9 1.7c.5 1.8.7 4.4 2.1 4.4.7 0 1.3-.5 1.7-1.6.6-1.7.7-4.4 1.3-6.1.6-1.6 1.5-2.8 1.5-5 0-2.6-1.4-4.6-4-4.6-1.8 0-2.7 1-4.5 1s-2.7-1-4.5-1z"/></svg>',
  menu: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12h18a9 9 0 0 1-18 0z"/><path d="M8 8c0-1.5 1-1.5 1-3M12 8c0-1.5 1-1.5 1-3M16 8c0-1.5 1-1.5 1-3"/></svg>',
  conquete: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 21V10h3V7h2v3h2V4h2v6h2V7h2v3h3v11z"/><path d="M10 21v-4a2 2 0 0 1 4 0v4"/><path d="M12 4V1.5l3 1-3 1"/></svg>',
  suite: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2.5 3.5 7v10l8.5 4.5 8.5-4.5V7z"/><path d="M3.5 7 12 11.5 20.5 7M12 11.5v10"/></svg>',
  defaut: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 6.5h.01M10 6.5h.01"/></svg>',
};

const PHASES = {
  en_marche: ["En marche", "ok"],
  demarrage: ["Démarrage", "attente"],
  arret: ["Arrêt", "attente"],
  maj: ["Mise à jour", "attente"],
  installation: ["Installation", "attente"],
  arrete: ["Arrêté", "eteint"],
  erreur: ["Erreur", "erreur"],
  externe: ["Lancé hors launcher", "info"],
};

let etat = null;
let menuOuvert = null;
let journalOuvert = null;
const signatures = {};

const $ = (s, r = document) => r.querySelector(s);
const echap = (t) => String(t ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

function toast(texte, erreur = false) {
  const t = $("#toast");
  t.textContent = texte;
  t.className = "toast visible" + (erreur ? " erreur" : "");
  clearTimeout(toast.minuteur);
  toast.minuteur = setTimeout(() => (t.className = "toast"), 3500);
}

async function poster(url) {
  const r = await fetch(url, { method: "POST", headers: { "X-Launcher": "1" } });
  const d = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(d.erreur || "Erreur " + r.status);
  return d;
}

// Depuis le Pi lui-même, 127.0.0.1 : WEB·SUITE reconnaît « ce poste » et ne demande pas de code.
function hoteOutils() {
  const h = location.hostname;
  if (h === "localhost" || h.startsWith("127.") || h === "::1" || h === "[::1]") return "127.0.0.1";
  return (etat && etat.launcher.ip) || h;
}
const urlOutil = (o) => `http://${hoteOutils()}:${o.port}/`;

function dateCourte(iso) {
  if (!iso) return "";
  return new Date(iso).toLocaleDateString("fr-FR", { day: "numeric", month: "short", year: "numeric" });
}
function duree(secondes) {
  if (secondes < 60) return "à l'instant";
  const m = Math.floor(secondes / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h} h ${String(m % 60).padStart(2, "0")}`;
  return `${Math.floor(h / 24)} j`;
}
const ilYa = (s) => (s < 60 ? "à l'instant" : "il y a " + duree(s));

/* -- rendu ------------------------------------------------------------- */

function rendreEntete() {
  const l = etat.launcher;
  $("#sous-titre").textContent = `${l.nom_machine} · ${l.ip || "hors réseau"}`;
  const html = l.adresses.map((a) => `
    <a class="adresse" href="${echap(a.url)}">
      <small>${echap(a.type)}</small><code>${echap(a.url.replace(/^http:\/\//, "").replace(/\/$/, ""))}</code>
      <button class="copier" data-copier="${echap(a.url)}" title="Copier">copier</button>
    </a>`).join("");
  if ($("#adresses").dataset.sig !== html) {
    $("#adresses").innerHTML = html;
    $("#adresses").dataset.sig = html;
  }
  const n = etat.outils.length;
  const allumes = etat.outils.filter((o) => o.phase === "en_marche").length;
  $("#compte").textContent = `${n} outils · ${allumes} en marche`;

  const v = l.version;
  const m = l.maj || {};
  const pied = `
    <span class="etiquette">Launcher</span>
    <span>Version <code>${v ? echap(v.commit) : "?"}</code>${v ? " · " + dateCourte(v.date) : ""}</span>
    <span>Outils dans <code>${echap(l.dossier_outils)}</code></span>
    <span>launcher.local : <code>${echap(l.mdns)}</code></span>
    <span class="espace"></span>
    ${m.erreur ? `<span class="message erreur">${echap(m.erreur)}</span>` : ""}
    ${l.action ? `<span class="statut attente">${echap(l.action)}</span>`
      : m.nombre ? `<button class="bouton accent" data-launcher="mettre_a_jour">Mettre à jour le launcher (${m.nombre})</button>`
      : `<button class="lien" data-launcher="verifier">Vérifier sa mise à jour</button>`}`;
  if ($("#pied").dataset.sig !== pied) {
    $("#pied").innerHTML = pied;
    $("#pied").dataset.sig = pied;
  }
}

function boutonsPrincipaux(o) {
  const occupe = !!o.action;
  const dis = occupe ? " disabled" : "";
  const b = [];
  if (!o.installe) {
    b.push(`<button class="bouton" data-action="installer"${dis}>${occupe ? echap(o.action) + "…" : "Installer"}</button>`);
    return b.join("");
  }
  if (o.phase === "en_marche" || o.phase === "externe" || (o.phase === "demarrage" && o.pid)) {
    b.push(`<a class="bouton" href="${urlOutil(o)}" target="_blank" rel="noopener">Ouvrir ↗</a>`);
  }
  if (o.maj && o.maj.nombre) {
    b.push(`<button class="bouton accent" data-action="mettre_a_jour"${dis}>Mettre à jour</button>`);
  }
  if (occupe) {
    b.push(`<button class="bouton secondaire" disabled>${echap(o.action)}…</button>`);
  } else if (o.pid) {
    b.push(`<button class="bouton danger" data-action="arreter">Arrêter</button>`);
  } else if (o.phase !== "externe") {
    b.push(`<button class="bouton" data-action="demarrer">Démarrer</button>`);
  }
  return b.join("");
}

function menuPlus(o) {
  const occupe = !!o.action;
  const d = (cond) => (cond ? "" : " disabled");
  return `
    <div class="menu-plus${menuOuvert === o.id ? " ouvert" : ""}">
      <button data-menu="${o.id}" aria-label="Plus d'actions">…</button>
      <div class="liste">
        <button data-action="redemarrer"${d(o.installe && !occupe && o.phase !== "externe")}>Redémarrer</button>
        <button data-action="verifier"${d(o.installe && !occupe)}>Vérifier la mise à jour</button>
        <button data-action="mettre_a_jour"${d(o.installe && !occupe)}>Mettre à jour maintenant</button>
        <button data-terminal="${o.id}">Terminal ↗</button>
        <button data-journal="${o.id}">Voir le journal</button>
        <button data-copier="${echap(urlOutil(o))}">Copier l'adresse</button>
      </div>
    </div>`;
}

function carte(o, i) {
  const [libelle, classe] = PHASES[o.phase] || [o.phase, "eteint"];
  const statuts = [];
  if (!o.installe) statuts.push(`<span class="statut info">Disponible à l'installation</span>`);
  else statuts.push(`<span class="statut ${classe}">${libelle}</span>`);
  if (o.maj && o.maj.nombre) statuts.push(`<span class="statut maj">${o.maj.nombre} mise${o.maj.nombre > 1 ? "s" : ""} à jour</span>`);
  if (o.attend_saisie) statuts.push(`<button class="statut maj" data-terminal="${o.id}" title="Ouvrir son terminal">Attend une réponse ↗</button>`);
  if (o.installe && o.voulu && o.pid) statuts.push(`<span class="statut eteint" title="Rallumé automatiquement au démarrage du Raspberry Pi">Auto</span>`);

  const v = o.version;
  const infos = [];
  if (o.installe) {
    infos.push(`<div class="ligne"><span>Adresse</span><code>${echap(urlOutil(o))}</code>
      <button class="copier" data-copier="${echap(urlOutil(o))}">copier</button></div>`);
    if (v) infos.push(`<div class="ligne"><span>Version</span><span class="tronque" title="${echap(v.sujet)}"><code>${echap(v.commit)}</code> · ${dateCourte(v.date)} · ${echap(v.sujet)}</span></div>`);
    if (o.pid && o.depuis) infos.push(`<div class="ligne"><span>Depuis</span><span>${duree(Math.max(0, etat.launcher.heure - o.depuis))} · PID ${o.pid}</span></div>`);
    if (o.sous_depots.length) infos.push(`<div class="ligne"><span>Contient</span><span>${o.sous_depots.map(echap).join(", ")}</span></div>`);
    if (o.maj && o.maj.verifie_le) {
      const age = ilYa(Math.max(0, etat.launcher.heure - o.maj.verifie_le));
      const texte = o.maj.erreur ? `<span class="message erreur">${echap(o.maj.erreur)}</span>`
        : o.maj.nombre ? `${o.maj.nombre} nouveauté(s)` : "à jour";
      infos.push(`<div class="ligne"><span>GitHub</span><span>${texte} · vérifié ${age}</span></div>`);
    }
  } else {
    infos.push(`<div class="ligne"><span>Dossier</span><code>${echap(o.dossier)}</code></div>`);
    infos.push(`<div class="ligne"><span>Port</span><code>${o.port}</code></div>`);
  }

  const nouveautes = o.maj && o.maj.nombre && o.maj.nouveautes.length
    ? `<ul class="nouveautes">${o.maj.nouveautes.slice(0, 4).map((n) => `<li>${echap(n)}</li>`).join("")}${o.maj.nombre > 4 ? `<li>…</li>` : ""}</ul>` : "";

  const code = o.code_appairage
    ? `<div class="appairage">Code d'appairage <strong>${echap(o.code_appairage.code)}</strong>
       <span>à saisir sur la tablette / le téléphone, valable 10 min (affiché ${ilYa(o.code_appairage.age)})</span></div>` : "";

  const erreur = o.phase === "erreur" || /impossible|échou|refus|bloqu|pris par/i.test(o.message || "");
  return `
    <article class="carte ${o.phase}${o.installe ? " installe" : ""}" style="--accent:${echap(o.accent)}" data-id="${o.id}">
      <div class="carte-tete">
        <span class="numero">${String(i + 1).padStart(2, "0")}</span>
        <span class="etiquette">:${o.port}</span>
        <div class="liens">
          ${o.installe && (o.pid || o.phase === "externe") ? `<a href="${urlOutil(o)}" target="_blank" rel="noopener">Ouvrir ↗</a>` : ""}
          <a href="${echap(o.site)}" target="_blank" rel="noopener">GitHub ↗</a>
        </div>
      </div>
      <div class="carte-corps">
        <div class="identite">
          <div class="icone">${ICONES[o.icone] || ICONES.defaut}</div>
          <div>
            <h2>${echap(o.nom)}<span>${echap(o.suffixe)}</span></h2>
            <p>${echap(o.description)}</p>
          </div>
        </div>
        <div class="statuts">${statuts.join("")}</div>
        <div class="infos">${infos.join("")}</div>
        ${nouveautes}
        ${code}
        <div class="message${erreur ? " erreur" : ""}">${echap(o.message)}</div>
        <div class="actions">${boutonsPrincipaux(o)}${menuPlus(o)}</div>
      </div>
    </article>`;
}

function rendre() {
  rendreEntete();
  const grille = $("#grille");
  etat.outils.forEach((o, i) => {
    const html = carte(o, i);
    let el = grille.querySelector(`[data-id="${o.id}"]`);
    if (signatures[o.id] === html && el) return;
    signatures[o.id] = html;
    const tmp = document.createElement("div");
    tmp.innerHTML = html.trim();
    if (el) el.replaceWith(tmp.firstChild);
    else grille.appendChild(tmp.firstChild);
  });
}

async function actualiser() {
  try {
    const r = await fetch("/api/etat", { cache: "no-store" });
    etat = await r.json();
    rendre();
  } catch (e) {
    $("#sous-titre").textContent = "Launcher injoignable… nouvelle tentative";
  }
}

/* -- journal ------------------------------------------------------------ */

async function chargerJournal() {
  if (!journalOuvert) return;
  const pre = $("#journal-texte");
  const enBas = pre.scrollHeight - pre.scrollTop - pre.clientHeight < 40;
  const r = await fetch(`/api/journal/${journalOuvert}?lignes=800`, { cache: "no-store" });
  pre.textContent = await r.text();
  if (enBas) pre.scrollTop = pre.scrollHeight;
}
function ouvrirJournal(id) {
  const o = etat.outils.find((x) => x.id === id);
  journalOuvert = id;
  $("#journal-titre").textContent = `Journal · ${o.nom}${o.suffixe}`;
  $("#journal-texte").textContent = "Chargement…";
  $("#journal").showModal();
  chargerJournal().then(() => { const p = $("#journal-texte"); p.scrollTop = p.scrollHeight; });
}
$("#journal-fermer").onclick = () => $("#journal").close();
$("#journal-actualiser").onclick = chargerJournal;
$("#journal").addEventListener("close", () => (journalOuvert = null));

/* -- clics ------------------------------------------------------------- */

const CONFIRMER = {
  arreter: (o) => `Arrêter ${o.nom}${o.suffixe} ?`,
  redemarrer: (o) => `Redémarrer ${o.nom}${o.suffixe} ?`,
  mettre_a_jour: (o) => o.pid ? `Mettre à jour ${o.nom}${o.suffixe} ? L'outil va redémarrer.` : null,
  installer: (o) => `Télécharger ${o.nom}${o.suffixe} depuis GitHub dans ${o.dossier} ?`,
};

document.addEventListener("click", async (ev) => {
  const cible = ev.target.closest("button, a");
  if (!cible) {
    if (menuOuvert) { menuOuvert = null; rendre(); }
    return;
  }
  if (cible.dataset.copier) {
    ev.preventDefault();
    ev.stopPropagation();
    const texte = cible.dataset.copier;
    try { await navigator.clipboard.writeText(texte); }
    catch {                                  // http:// hors localhost : pas d'API presse-papiers
      const t = document.createElement("textarea");
      t.value = texte; document.body.appendChild(t); t.select();
      document.execCommand("copy"); t.remove();
    }
    toast("Adresse copiée : " + texte);
    if (menuOuvert) { menuOuvert = null; rendre(); }
    return;
  }
  if (cible.dataset.menu) {
    menuOuvert = menuOuvert === cible.dataset.menu ? null : cible.dataset.menu;
    rendre();
    return;
  }
  if (cible.dataset.terminal !== undefined) {
    // une seule fenêtre « Terminaux », réutilisée : l'onglet de l'outil y est choisi
    menuOuvert = null;
    const f = window.open("/console.html#" + cible.dataset.terminal, "terminaux", "popup=yes,width=1100,height=720");
    if (f) f.focus();
    rendre();
    return;
  }
  if (cible.dataset.journal) {
    menuOuvert = null;
    ouvrirJournal(cible.dataset.journal);
    rendre();
    return;
  }
  if (cible.dataset.launcher) {
    const action = cible.dataset.launcher;
    if (action === "mettre_a_jour" && !confirm("Mettre à jour le launcher ? Il redémarre quelques secondes, les outils continuent de tourner.")) return;
    try {
      const r = await poster(`/api/launcher/${action}`);
      toast(r.message || (action === "verifier" ? (r.maj.erreur || (r.maj.nombre ? `${r.maj.nombre} mise(s) à jour disponible(s)` : "Le launcher est à jour")) : "OK"), !!(r.maj && r.maj.erreur));
    } catch (e) { toast(e.message, true); }
    actualiser();
    return;
  }
  if (cible.dataset.action) {
    const el = cible.closest("[data-id]");
    const o = etat.outils.find((x) => x.id === el.dataset.id);
    const action = cible.dataset.action;
    menuOuvert = null;
    const question = CONFIRMER[action] && CONFIRMER[action](o);
    if (question && !confirm(question)) { rendre(); return; }
    try {
      await poster(`/api/outils/${o.id}/${action}`);
    } catch (e) { toast(e.message, true); }
    actualiser();
  }
});

$("#verifier-tout").onclick = async () => {
  try { await poster("/api/verifier_tout"); toast("Recherche des mises à jour sur GitHub…"); }
  catch (e) { toast(e.message, true); }
  actualiser();
};

actualiser();
setInterval(() => { if (!document.hidden) actualiser(); }, 2000);
setInterval(() => { if (journalOuvert) chargerJournal(); }, 3000);
document.addEventListener("visibilitychange", () => { if (!document.hidden) actualiser(); });
