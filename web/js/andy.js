(async function () {
  "use strict";

  /* ===== Données et constantes ===== */
  var BASE = [];          // l'historique : /api/andy/donnees (andy_donnees.local.json, sur le Pi)
  var pesees = [];        // les pesées ajoutées depuis la page : gardées par le launcher (/api/andy/pesees)
  var DATA = [];          // les deux, à afficher
  function peseeEvt(p) {
    return { date: p.date, type: "poids", titre: "Pesée", resume: String(p.poids).replace(".", ",") + " kg", poids: Number(p.poids),
      perso: true, id: p.id, details: ["Pesée ajoutée depuis cette page."], source: "Saisie manuelle" };
  }
  var evenements = [];    // les évènements ajoutés depuis la page (médicament, visite…) : /api/andy/evenements
  function evenementEvt(e) {
    return { date: e.date, type: e.type, titre: e.titre, resume: e.note || "", perso: true, id: e.id,
      details: [], source: "Saisie manuelle" };
  }
  async function api(chemin, corps) {
    var r = await fetch("/api/andy/" + chemin, corps === undefined ? { cache: "no-store" } :
      { method: "POST", headers: { "X-Launcher": "1", "Content-Type": "application/json" }, body: JSON.stringify(corps) });
    if (r.status === 401) { location.href = "/connexion.html"; throw new Error("connexion requise"); }
    var d = await r.json();
    if (!r.ok) throw new Error(d.erreur || "erreur");
    return d;
  }
  async function chargerPesees() { pesees = (await api("pesees")).pesees; }
  async function chargerEvenements() { evenements = (await api("evenements")).evenements; }
  function assembler() {
    DATA = BASE.concat(pesees.map(peseeEvt), evenements.map(evenementEvt));
    DATA.forEach(function (e, i) { e._i = i; indexer(e); });
  }
  var NAISSANCE = new Date(2025, 10, 30, 12);
  var TYPES = {
    naissance:       { label: "Naissance",              c: "--c-naissance" },
    vaccin:          { label: "Vaccin",                 c: "--c-vaccin" },
    vermifuge:       { label: "Vermifuge",              c: "--c-vermifuge" },
    antiparasitaire: { label: "Anti-tiques et puces",   c: "--c-antiparasitaire" },
    consultation:    { label: "Consultation",           c: "--c-consultation" },
    medicament:      { label: "Médicament",             c: "--c-medicament" },
    soin:            { label: "Soin et intervention",   c: "--c-soin" },
    osteo:           { label: "Ostéopathie",            c: "--c-osteo" },
    poids:           { label: "Pesée",                  c: "--c-poids" },
    delivrance:      { label: "Délivrance clinique",    c: "--c-delivrance" },
    document:        { label: "Document au dossier",    c: "--c-document" },
    rdv:             { label: "À venir",                c: "--c-rdv" }
  };
  var AUJOURDHUI = new Date(); AUJOURDHUI.setHours(12, 0, 0, 0);

  var etat = { q: "", actifs: new Set(Object.keys(TYPES)), tri: "desc", ouverts: new Set(), tout: false };

  /* ===== Utilitaires ===== */
  function d(iso) { return new Date(iso + "T12:00:00"); }
  function fmt(iso) { var x = iso.split("-"); return x[2] + "/" + x[1] + "/" + x[0]; }
  function jours(a, b) { return Math.round((b - a) / 86400000); }
  function esc(s) { return String(s).replace(/[&<>"']/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]; }); }
  function fold(s) {
    var out = "";
    for (var i = 0; i < s.length; i++) {
      var n = s[i].normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();
      out += n.length === 1 ? n : s[i];
    }
    return out;
  }
  function age(date) {
    var j = jours(NAISSANCE, date);
    if (j < 0) return "";
    if (j < 56) { return Math.floor(j / 7) + " sem."; }
    var m = (date.getFullYear() - NAISSANCE.getFullYear()) * 12 + date.getMonth() - NAISSANCE.getMonth() - (date.getDate() < NAISSANCE.getDate() ? 1 : 0);
    return m < 12 ? m + " mois" : "1 an";
  }
  function relatif(date) {
    var j = jours(date, AUJOURDHUI);
    if (j === 0) return "aujourd'hui";
    if (j > 0) return "il y a " + j + " j";
    return "dans " + (-j) + " j";
  }
  function termes() { return fold(etat.q).split(/\s+/).filter(Boolean); }

  /* Surlignage insensible aux accents et à la casse */
  function hl(texte) {
    texte = String(texte == null ? "" : texte);
    var t = termes();
    if (!t.length) return esc(texte);
    var f = fold(texte), marques = new Array(texte.length).fill(false);
    t.forEach(function (mot) {
      var p = f.indexOf(mot);
      while (p !== -1) { for (var k = p; k < p + mot.length; k++) marques[k] = true; p = f.indexOf(mot, p + 1); }
    });
    var r = "", ouvert = false;
    for (var i = 0; i < texte.length; i++) {
      if (marques[i] && !ouvert) { r += "<mark>"; ouvert = true; }
      if (!marques[i] && ouvert) { r += "</mark>"; ouvert = false; }
      r += esc(texte[i]);
    }
    return r + (ouvert ? "</mark>" : "");
  }

  /* Texte de recherche de chaque événement */
  function indexer(e) {
    var dt = d(e.date);
    var morceaux = [
      e.titre, e.resume, (e.details || []).join(" "), e.carnet, e.cout, e.praticien, e.lieu, e.source, e.heure,
      TYPES[e.type].label, fmt(e.date), e.date,
      dt.toLocaleDateString("fr-FR", { day: "numeric", month: "long", year: "numeric" }),
      age(dt), e.poids ? e.poids + " kg" : ""
    ];
    (e.produits || []).forEach(function (p) { morceaux.push(p.nom, p.detail); });
    e._texte = fold(morceaux.filter(Boolean).join(" \n "));
  }

  function visibles() {
    var t = termes();
    return DATA.filter(function (e) {
      return etat.actifs.has(e.type) && t.every(function (mot) { return e._texte.indexOf(mot) !== -1; });
    }).sort(function (a, b) {
      var k = a.date < b.date ? -1 : a.date > b.date ? 1 : a._i - b._i;
      return etat.tri === "asc" ? k : -k;
    });
  }

  /* ===== Frise ===== */
  function carte(e, ouvert) {
    var T = TYPES[e.type], dt = d(e.date), futur = dt > AUJOURDHUI;
    var h = '<li class="evt' + (futur ? " futur" : "") + '" style="--c:var(' + T.c + ')" data-id="' + e._i + '" data-ouvert="' + ouvert + '">';
    h += '<div class="evt-tete"><div class="meta"><span class="date">' + fmt(e.date) + "</span>";
    if (e.heure) h += "<span>" + hl(e.heure) + "</span>";
    var a = age(dt); if (a) h += "<span>" + esc(a) + "</span>";
    h += '<span class="badge">' + hl(T.label) + "</span>";
    if (e.poids) h += "<span>" + hl(String(e.poids).replace(".", ",")) + " kg</span>";
    if (futur) h += "<span>" + relatif(dt) + "</span>";
    h += '<span class="chevron" aria-hidden="true">▾</span></div>';
    h += '<h3><button type="button" aria-expanded="' + ouvert + '">' + hl(e.titre) + "</button></h3>";
    if (e.resume) h += '<p class="resume">' + hl(e.resume) + "</p>";
    h += "</div>";
    if (e.produits && e.produits.length) {
      h += '<div class="etiquettes">' + e.produits.map(function (p) { return '<span class="etiquette">' + hl(p.nom) + "</span>"; }).join("") + "</div>";
    }
    h += '<div class="corps"' + (ouvert ? "" : " hidden") + ">";
    if (e.carnet) h += '<p class="carnet">' + hl(e.carnet) + "</p>";
    if (e.details && e.details.length) h += "<ul>" + e.details.map(function (x) { return "<li>" + hl(x) + "</li>"; }).join("") + "</ul>";
    if (e.produits && e.produits.length) {
      h += '<table class="produits"><tbody>' + e.produits.map(function (p) {
        return "<tr><th scope=\"row\">" + hl(p.nom) + "</th><td>" + hl(p.detail || "") + "</td></tr>";
      }).join("") + "</tbody></table>";
    }
    h += '<dl class="faits">';
    if (e.praticien) h += "<dt>Praticien</dt><dd>" + hl(e.praticien) + "</dd>";
    if (e.lieu) h += "<dt>Lieu</dt><dd>" + hl(e.lieu) + "</dd>";
    if (e.cout) h += "<dt>Coût</dt><dd>" + hl(e.cout) + "</dd>";
    if (e.source) h += "<dt>Source</dt><dd>" + hl(e.source) + "</dd>";
    h += "</dl>" + (e.perso ? '<button type="button" class="bouton suppr" data-id="' + esc(e.id) + '">Supprimer</button>' : "") + "</div></li>";
    return h;
  }

  function rendre() {
    var liste = visibles();
    var auto = termes().length > 0 && liste.length <= 8;
    var html = "", mois = null, ouvertListe = false;
    liste.forEach(function (e) {
      var m = e.date.slice(0, 7);
      if (m !== mois) {
        if (ouvertListe) html += "</ol>";
        html += '<h2 class="mois">' + esc(d(e.date).toLocaleDateString("fr-FR", { month: "long", year: "numeric" })) + '</h2><ol class="liste">';
        mois = m; ouvertListe = true;
      }
      html += carte(e, etat.tout || auto || etat.ouverts.has(e._i));
    });
    if (ouvertListe) html += "</ol>";
    if (!liste.length) {
      html = '<div class="vide">Aucun événement ne correspond. Essayez un autre mot (par exemple nexgard, oreille ou 2026-10) ou réactivez des types dans les filtres.</div>';
    }
    document.getElementById("frise").innerHTML = html;
    document.getElementById("compte").textContent = liste.length + " sur " + DATA.length + " événements";
    document.getElementById("effacer").hidden = !etat.q;
    document.getElementById("tri").textContent = etat.tri === "desc" ? "Du plus récent" : "Du plus ancien";
    document.getElementById("deplier").textContent = etat.tout ? "Tout replier" : "Tout déplier";
  }

  /* ===== Panneau latéral ===== */
  function panneau() {
    var passes = DATA.filter(function (e) { return d(e.date) <= AUJOURDHUI; });
    var nb = function (t) { return passes.filter(function (e) { return e.type === t; }).length; };
    var cases = [
      [nb("vaccin"), "bilans et vaccins"], [nb("vermifuge"), "vermifuges"],
      [nb("antiparasitaire"), "anti-tiques et puces"], [nb("consultation") + nb("soin"), "consultations et soins"]
    ];
    document.getElementById("chiffres").innerHTML = cases.map(function (c) {
      return '<div class="chiffre"><b>' + c[0] + "</b><span>" + c[1] + "</span></div>";
    }).join("");

    var dernier = function (types) {
      return passes.filter(function (e) { return types.indexOf(e.type) !== -1; })
        .sort(function (a, b) { return a.date < b.date ? 1 : -1; })[0];
    };
    var lignes = [["Dernier vermifuge", dernier(["vermifuge"])], ["Dernier anti-tiques", dernier(["antiparasitaire"])], ["Dernière consultation", dernier(["consultation", "soin"])], ["Dernière ostéo", dernier(["osteo"])]];
    document.getElementById("dernieres").innerHTML = lignes.filter(function (l) { return l[1]; }).map(function (l) {
      return "<li><span>" + l[0] + " : " + fmt(l[1].date) + "</span><em>" + relatif(d(l[1].date)) + "</em></li>";
    }).join("");

    var pesees = DATA.filter(function (e) { return e.poids; }).sort(function (a, b) { return a.date < b.date ? -1 : 1; });
    var der = pesees[pesees.length - 1];
    document.getElementById("dernier-poids").textContent = der ? String(der.poids).replace(".", ",") + " kg (" + fmt(der.date) + ")" : "—";
    document.getElementById("age-andy").textContent = "Cocker anglais · " + (function () {
      var j = jours(NAISSANCE, AUJOURDHUI), m = Math.floor(j / 30.4375);
      return m < 12 ? m + " mois" : "1 an";
    })();

    var futurs = DATA.filter(function (e) { return d(e.date) > AUJOURDHUI; }).sort(function (a, b) { return a.date < b.date ? -1 : 1; });
    document.getElementById("avenir").innerHTML = futurs.length ? futurs.map(function (e) {
      return "<li>" + esc(e.titre) + "<small>" + fmt(e.date) + " · " + relatif(d(e.date)) + "</small></li>";
    }).join("") : "<li>Aucun rendez-vous noté.</li>";

    courbe(pesees);
  }

  function courbe(pts) {
    var W = 300, H = 170, g = 34, droite = 14, haut = 16, bas = 28;
    if (pts.length < 2) { document.getElementById("courbe").textContent = "Pas assez de pesées."; return; }
    var t0 = d(pts[0].date).getTime() - 10 * 86400000, t1 = d(pts[pts.length - 1].date).getTime() + 12 * 86400000;
    var max = Math.ceil(Math.max.apply(null, pts.map(function (p) { return p.poids; })) / 5) * 5;
    var X = function (t) { return g + (t - t0) / (t1 - t0) * (W - g - droite); };
    var Y = function (v) { return haut + (1 - v / max) * (H - haut - bas); };
    var s = '<svg viewBox="0 0 ' + W + " " + H + '" role="img" aria-label="Courbe de poids d\'Andy">';
    for (var v = 0; v <= max; v += 5) {
      s += '<line class="axe" x1="' + g + '" x2="' + (W - droite) + '" y1="' + Y(v) + '" y2="' + Y(v) + '"/><text x="' + (g - 6) + '" y="' + (Y(v) + 3) + '" text-anchor="end">' + v + "</text>";
    }
    s += '<polyline class="ligne" points="' + pts.map(function (p) { return X(d(p.date).getTime()).toFixed(1) + "," + Y(p.poids).toFixed(1); }).join(" ") + '"/>';
    pts.forEach(function (p, i) {
      var x = X(d(p.date).getTime()), y = Y(p.poids);
      s += '<circle class="pt" cx="' + x.toFixed(1) + '" cy="' + y.toFixed(1) + '" r="4"><title>' + fmt(p.date) + " : " + String(p.poids).replace(".", ",") + " kg</title></circle>";
      s += '<text class="val" x="' + x.toFixed(1) + '" y="' + (y - 9).toFixed(1) + '" text-anchor="' + (i === pts.length - 1 ? "end" : "middle") + '">' + String(p.poids).replace(".", ",") + "</text>";
      s += '<text x="' + x.toFixed(1) + '" y="' + (H - 10) + '" text-anchor="middle">' + fmt(p.date).slice(0, 5) + "</text>";
    });
    document.getElementById("courbe").innerHTML = s + "</svg>";
  }

  /* ===== Interactions ===== */
  var champ = document.getElementById("q");
  var minuteur;
  champ.addEventListener("input", function () {
    clearTimeout(minuteur);
    minuteur = setTimeout(function () { etat.q = champ.value.trim(); rendre(); }, 120);
  });
  document.getElementById("effacer").addEventListener("click", function () { champ.value = ""; etat.q = ""; rendre(); champ.focus(); });
  document.addEventListener("keydown", function (ev) {
    var tag = (document.activeElement || {}).tagName;
    if (ev.key === "/" && tag !== "INPUT") { ev.preventDefault(); champ.focus(); }
    if (ev.key === "Escape" && document.activeElement === champ) { champ.value = ""; etat.q = ""; rendre(); }
  });

  var puces = document.getElementById("puces");
  Object.keys(TYPES).forEach(function (k) {
    var b = document.createElement("button");
    b.type = "button"; b.className = "puce"; b.style.setProperty("--c", "var(" + TYPES[k].c + ")");
    b.setAttribute("aria-pressed", "true"); b.dataset.type = k; b.textContent = TYPES[k].label;
    puces.appendChild(b);
  });
  puces.addEventListener("click", function (ev) {
    var b = ev.target.closest(".puce"); if (!b) return;
    var k = b.dataset.type, tous = etat.actifs.size === Object.keys(TYPES).length;
    if (tous) { etat.actifs = new Set([k]); }                       // premier clic : isole le type
    else if (etat.actifs.has(k)) { etat.actifs.delete(k); if (!etat.actifs.size) etat.actifs = new Set(Object.keys(TYPES)); }
    else { etat.actifs.add(k); }
    Array.prototype.forEach.call(puces.children, function (c) { c.setAttribute("aria-pressed", String(etat.actifs.has(c.dataset.type))); });
    rendre();
  });

  document.getElementById("tri").addEventListener("click", function () { etat.tri = etat.tri === "desc" ? "asc" : "desc"; rendre(); });
  document.getElementById("deplier").addEventListener("click", function () { etat.tout = !etat.tout; etat.ouverts.clear(); rendre(); });

  document.getElementById("frise").addEventListener("click", async function (ev) {
    if (ev.target.closest("a")) return;
    var s = ev.target.closest(".suppr");
    if (s) {
      if (!confirm("Supprimer cette entrée ?")) return;
      try {
        await api("pesees/supprimer", { id: s.dataset.id });     // l'id suffit : le serveur cherche aussi dans les évènements
        await Promise.all([chargerPesees(), chargerEvenements()]);
        etat.ouverts.clear(); assembler(); panneau(); rendre();
      } catch (x) { document.getElementById("pesee-msg").textContent = x.message; }
      return;
    }
    var tete = ev.target.closest(".evt-tete"); if (!tete) return;
    var li = tete.closest(".evt"), id = Number(li.dataset.id);
    var ouvert = li.dataset.ouvert !== "true";
    li.dataset.ouvert = ouvert;
    li.querySelector(".corps").hidden = !ouvert;
    li.querySelector("h3 button").setAttribute("aria-expanded", String(ouvert));
    if (ouvert) etat.ouverts.add(id); else { etat.ouverts.delete(id); etat.tout = false; document.getElementById("deplier").textContent = "Tout déplier"; }
  });

  document.getElementById("export").addEventListener("click", function () {
    var copie = DATA.map(function (e) { var c = Object.assign({}, e); delete c._i; delete c._texte; return c; });
    var url = URL.createObjectURL(new Blob([JSON.stringify(copie, null, 2)], { type: "application/json" }));
    var a = document.createElement("a"); a.href = url; a.download = "andy_historique.json"; a.click();
    setTimeout(function () { URL.revokeObjectURL(url); }, 1000);
  });

  document.getElementById("theme").addEventListener("click", function () {
    var r = document.documentElement, sombre = getComputedStyle(r).getPropertyValue("--papier").trim() === "#14171e";
    r.setAttribute("data-theme", sombre ? "clair" : "sombre");
  });

  var form = document.getElementById("pesee"), msg = document.getElementById("pesee-msg");
  function jourLocal() { var n = new Date(); return n.getFullYear() + "-" + String(n.getMonth() + 1).padStart(2, "0") + "-" + String(n.getDate()).padStart(2, "0"); }
  form.elements.date.min = "2025-11-30"; form.elements.date.max = jourLocal(); form.elements.date.value = jourLocal();
  form.addEventListener("submit", async function (ev) {
    ev.preventDefault();
    var kg = Math.round(parseFloat(form.elements.kg.value) * 100) / 100;
    if (!form.elements.date.value || form.elements.date.value < form.elements.date.min || form.elements.date.value > form.elements.date.max || !(kg >= 0.5 && kg <= 100)) {
      msg.textContent = "Date ou poids invalide."; return;
    }
    try {
      var p = (await api("pesees", { date: form.elements.date.value, poids: kg })).pesee;
      await chargerPesees();
      msg.textContent = "Pesée du " + fmt(p.date) + " ajoutée : " + String(p.poids).replace(".", ",") + " kg.";
      form.elements.kg.value = ""; assembler(); panneau(); rendre();
    } catch (x) { msg.textContent = x.message; }
  });

  var fe = document.getElementById("evenement"), msgE = document.getElementById("evenement-msg");
  Object.keys(TYPES).forEach(function (k) {
    if (k === "naissance" || k === "poids" || k === "delivrance" || k === "document") return;   // non saisissables
    fe.elements.type.add(new Option(TYPES[k].label, k));
  });
  fe.elements.date.min = "2025-11-30"; fe.elements.date.value = jourLocal();
  fe.addEventListener("submit", async function (ev) {
    ev.preventDefault();
    try {
      var e = (await api("evenements", { date: fe.elements.date.value, type: fe.elements.type.value,
        titre: fe.elements.titre.value, note: fe.elements.note.value })).evenement;
      await chargerEvenements();
      msgE.textContent = "Évènement du " + fmt(e.date) + " ajouté.";
      fe.elements.titre.value = ""; fe.elements.note.value = ""; assembler(); panneau(); rendre();
    } catch (x) { msgE.textContent = x.message; }
  });

  try {
    BASE = await (await fetch("/api/andy/donnees", { cache: "no-store" })).json();
    await Promise.all([chargerPesees(), chargerEvenements()]);
  } catch (x) { document.getElementById("frise").textContent = "Chargement impossible : " + x.message; return; }
  assembler();
  panneau();
  rendre();
})();
