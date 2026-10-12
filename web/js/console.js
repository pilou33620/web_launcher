"use strict";

// Terminaux des web tools : ce que chaque outil affiche, en direct, et une ligne pour lui répondre.
// Les données viennent du journal de l'outil (/api/console/<id>), la saisie part vers son terminal.

const $ = (s) => document.querySelector(s);
const MAX_LIGNES = 4000;

let outils = [];
let actif = null;
const ecrans = {};      // id -> { position, lignes: [], colonne }
let enCours = false;

function toast(texte, erreur = false) {
  const t = $("#toast");
  t.textContent = texte;
  t.className = "toast visible" + (erreur ? " erreur" : "");
  clearTimeout(toast.m);
  toast.m = setTimeout(() => (t.className = "toast"), 3500);
}

async function poster(url, corps) {
  const r = await fetch(url, {
    method: "POST", headers: { "X-Launcher": "1", "Content-Type": "application/json" },
    body: corps ? JSON.stringify(corps) : undefined,
  });
  const d = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(d.erreur || "Erreur " + r.status);
  return d;
}

/* -- un petit terminal : séquences ANSI retirées, \r et retour arrière respectés -- */

const ANSI = /\x1b\[[0-?]*[ -\/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g;

function ecrire(e, texte) {
  texte = texte.replace(ANSI, "");
  let ligne = e.lignes.pop() ?? "";
  for (let i = 0; i < texte.length; i++) {
    const c = texte[i];
    if (c === "\n") { e.lignes.push(ligne); ligne = ""; e.colonne = 0; }
    else if (c === "\r") { if (texte[i + 1] !== "\n") e.colonne = 0; }
    else if (c === "\b") { e.colonne = Math.max(0, e.colonne - 1); }
    else if (c < " " && c !== "\t") { /* autre caractère de contrôle : ignoré */ }
    else {
      ligne = ligne.slice(0, e.colonne) + c + ligne.slice(e.colonne + 1);
      e.colonne++;
    }
  }
  e.lignes.push(ligne);
  if (e.lignes.length > MAX_LIGNES) e.lignes.splice(0, e.lignes.length - MAX_LIGNES);
}

function afficher() {
  const e = ecrans[actif];
  const ecran = $("#ecran");
  const enBas = ecran.scrollHeight - ecran.scrollTop - ecran.clientHeight < 40;
  ecran.textContent = e ? e.lignes.join("\n") : "";
  if ($("#suivre").checked && (enBas || e?.forcerBas)) ecran.scrollTop = ecran.scrollHeight;
  if (e) e.forcerBas = false;
}

/* -- onglets ------------------------------------------------------------- */

function rendreOnglets() {
  const html = outils.map((o) => {
    const etatPoint = o.phase === "en_marche" ? "ok" : ["demarrage", "arret", "maj", "installation"].includes(o.phase) ? "attente" : o.phase === "erreur" ? "erreur" : "";
    return `<button class="onglet" role="tab" data-id="${o.id}" style="--accent:${o.accent}" aria-selected="${o.id === actif}">
      <span class="point ${etatPoint}"></span>${o.nom}${o.suffixe}${o.attend_saisie ? ' <span class="question" title="Attend une réponse">?</span>' : ""}</button>`;
  }).join("");
  if ($("#onglets").dataset.sig !== html) {
    $("#onglets").innerHTML = html;
    $("#onglets").dataset.sig = html;
  }
}

function choisir(id) {
  if (!outils.some((o) => o.id === id)) id = outils[0]?.id;
  if (!id) return;
  actif = id;
  if (location.hash.slice(1) !== id) history.replaceState(null, "", "#" + id);
  const o = outils.find((x) => x.id === id);
  document.title = `${o.nom}${o.suffixe} · Terminaux`;
  document.body.style.setProperty("--accent", o.accent);
  if (ecrans[id]) ecrans[id].forcerBas = true;
  rendreOnglets();
  afficher();
  lire();
  $("#ligne").focus();
}

function bandeau(o, infos) {
  const b = $("#bandeau");
  const form = $("#saisie");
  const vivant = !!infos.pid && infos.terminal;
  form.setAttribute("aria-disabled", String(!vivant));
  $("#ligne").disabled = !vivant;
  form.querySelectorAll("button").forEach((x) => (x.disabled = !vivant));
  let html = "";
  let classe = "bandeau";
  if (!infos.pid) {
    classe += " eteint";
    html = o.installe ? `${o.nom}${o.suffixe} est arrêté. <button class="bouton" data-demarrer="${o.id}">Démarrer</button>`
      : `${o.nom}${o.suffixe} n'est pas installé (bouton Installer sur le launcher).`;
  } else if (!infos.terminal) {
    classe += " eteint";
    html = "Cet outil tourne sans terminal (lancé hors du launcher, ou terminal désactivé) : affichage seulement.";
  } else if (infos.attend_saisie) {
    html = "L'outil attend une réponse : la question est en bas du terminal. Tapez la réponse puis Entrée.";
  }
  b.className = classe;
  b.hidden = !html;
  if (b.dataset.sig !== html) { b.innerHTML = html; b.dataset.sig = html; }
}

/* -- lecture ------------------------------------------------------------- */

async function lire() {
  if (!actif || enCours) return;
  enCours = true;
  const id = actif;
  const e = (ecrans[id] ||= { position: -1, lignes: [""], colonne: 0, forcerBas: true });
  try {
    // plusieurs lots de suite si l'outil a beaucoup écrit
    for (let tour = 0; tour < 8; tour++) {
      const r = await fetch(`/api/console/${id}?depuis=${e.position}`, { cache: "no-store" });
      const d = await r.json();
      if (d.position < e.position) { e.lignes = [""]; e.colonne = 0; }   // journal renouvelé
      if (d.texte) ecrire(e, d.texte);
      const avance = d.position !== e.position;
      e.position = d.position;
      const o = outils.find((x) => x.id === id);
      if (o && id === actif) bandeau(o, d);
      if (!avance || !d.texte || d.texte.length < 30000) break;
    }
    if (id === actif) afficher();
  } catch (err) {
    /* launcher en redémarrage : nouvel essai au tour suivant */
  } finally {
    enCours = false;
  }
}

async function actualiserOutils() {
  try {
    const r = await fetch("/api/etat", { cache: "no-store" });
    outils = (await r.json()).outils;
    if (!actif) choisir(location.hash.slice(1));
    else rendreOnglets();
  } catch (err) { /* idem */ }
}

/* -- saisie ------------------------------------------------------------- */

async function envoyer(texte) {
  try {
    await poster(`/api/console/${actif}`, { texte });
    setTimeout(lire, 150);
  } catch (err) { toast(err.message, true); }
}

$("#saisie").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const ligne = $("#ligne");
  envoyer(ligne.value + "\r");
  ligne.value = "";
  if (ecrans[actif]) ecrans[actif].forcerBas = true;
});
$("#ctrl-c").onclick = () => {
  if (confirm("Envoyer Ctrl+C ? La plupart des outils s'arrêtent.")) envoyer("\x03");
};
$("#ligne").addEventListener("keydown", (ev) => {
  const l = $("#ligne");
  if (ev.ctrlKey && ev.key.toLowerCase() === "c" && l.selectionStart === l.selectionEnd) {
    ev.preventDefault();
    $("#ctrl-c").click();
  }
});

document.addEventListener("click", async (ev) => {
  const onglet = ev.target.closest(".onglet");
  if (onglet) return choisir(onglet.dataset.id);
  const dem = ev.target.closest("[data-demarrer]");
  if (dem) {
    try { await poster(`/api/outils/${dem.dataset.demarrer}/demarrer`); toast("Démarrage…"); }
    catch (err) { toast(err.message, true); }
  }
});
window.addEventListener("hashchange", () => choisir(location.hash.slice(1)));

actualiserOutils();
setInterval(() => { if (!document.hidden) lire(); }, 700);
setInterval(() => { if (!document.hidden) actualiserOutils(); }, 3000);
