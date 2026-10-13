// Who said it? — the page inside Discord.
//
// 1. Log in through Discord's SDK: Discord gives a one-time code, Vivy swaps it for an
//    access token (/api/token), and the SDK is told the token.
// 2. Open a WebSocket to Vivy and send the token and this window's instance ID. Vivy checks
//    both with Discord and puts us in the instance's game.
// 3. From then on Vivy sends the whole view whenever something changes, and we send
//    "start" and "pick". All the rules run in Vivy; this page only draws.
//
// Every request goes through Discord's proxy, which is why the paths start with /.proxy/.

import { DiscordSDK } from "./sdk.js";

const clientId = document.querySelector('meta[name="client-id"]').content;
const app = document.getElementById("app");

let sdk = null;
let socket = null;
let me = null;          // our user ID
let view = null;        // the latest view from Vivy
let myPick = null;      // what we picked this round, shown before the reveal
let pickRound = 0;      // the round `myPick` belongs to
let deadline = 0;       // when the question ends, in performance.now() time

main().catch((err) => {
  console.error(err);
  showMessage("Something went wrong while starting. Close the Activity and open it again.", true);
});

async function main() {
  // Discord opens the page with a frame_id; without one, it was opened in a plain browser
  // and the SDK would wait for Discord forever.
  if (!new URLSearchParams(location.search).has("frame_id")) {
    showMessage("Open this from Discord: it's an Activity, not a web page.", true);
    return;
  }
  sdk = new DiscordSDK(clientId);
  await sdk.ready();
  const { code } = await sdk.commands.authorize({
    client_id: clientId,
    response_type: "code",
    state: "",
    prompt: "none",
    scope: ["identify"],
  });
  const response = await fetch("/.proxy/api/token", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code }),
  });
  if (!response.ok) {
    showMessage("Logging in didn't work. Close the Activity and open it again.", true);
    return;
  }
  const { access_token } = await response.json();
  await sdk.commands.authenticate({ access_token });
  connect(access_token);
}

function connect(accessToken) {
  socket = new WebSocket(`wss://${location.host}/.proxy/ws`);
  socket.onopen = () => {
    socket.send(JSON.stringify({ access_token: accessToken, instance_id: sdk.instanceId }));
  };
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.type === "welcome") {
      me = message.you;
    } else if (message.type === "error") {
      showMessage(message.message, true);
      socket.onclose = null;
    } else if (message.type === "view") {
      view = message.view;
      if (view.round && view.round.ends_in_ms > 0) {
        deadline = performance.now() + view.round.ends_in_ms;
      }
      draw();
    }
  };
  // Lost the connection (Vivy restarted, the network blinked): try again in a moment.
  socket.onclose = () => {
    showMessage("Lost the connection to Vivy. Reconnecting…");
    setTimeout(() => connect(accessToken), 3000);
  };
}

function send(message) {
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(message));
  }
}

// ---- Drawing ----

function draw() {
  const screens = { lobby, loading, question, reveal: question, done };
  app.replaceChildren(...screens[view.phase]());
}

function lobby() {
  return [
    el("section", { class: "center" }, [
      el("h1", {}, ["Who said it", el("span", {}, ["?"])]),
      el("p", { class: "muted" }, [
        "Vivy digs up old messages from this channel. Pick who wrote each one, fast.",
      ]),
      players(),
      view.error ? el("p", { class: "error" }, [view.error]) : null,
      el("button", { class: "primary", onclick: () => send({ type: "start" }) }, ["Start"]),
    ]),
  ];
}

function loading() {
  return [
    el("section", { class: "center" }, [
      el("h1", {}, ["Who said it", el("span", {}, ["?"])]),
      el("p", { class: "muted" }, ["Vivy is digging through the archives…"]),
      players(),
    ]),
  ];
}

function question() {
  const round = view.round;
  const revealed = view.phase === "reveal";
  if (pickRound !== round.number) {
    pickRound = round.number;
    myPick = null;
  }
  const mine = revealed ? round.picks[me] ?? myPick : myPick;

  const choices = round.choices.map((choice) => {
    const classes = ["choice"];
    if (choice.id === mine) classes.push("mine");
    if (revealed && choice.id === round.answer) classes.push("right");
    else if (revealed && choice.id === mine) classes.push("wrong");
    const pickers = revealed
      ? view.players.filter((p) => round.picks[p.id] === choice.id).map((p) => avatar(p, true))
      : [];
    return el(
      "button",
      {
        class: classes.join(" "),
        disabled: revealed || mine !== null,
        onclick: () => {
          myPick = choice.id;
          send({ type: "pick", choice: choice.id });
          draw();
        },
      },
      [avatar(choice), el("span", { class: "name" }, [choice.name]), el("span", { class: "pickers" }, pickers)],
    );
  });

  const parts = [
    el("div", { class: "top" }, [
      el("span", {}, [`Question ${round.number} of ${round.total}`]),
      el("span", { id: "seconds" }, [""]),
    ]),
    el("div", { class: "timer" }, [el("div", { id: "bar", style: `width: ${timeLeft()}%` }, [])]),
    el("div", { class: "quote" }, [round.text, el("small", {}, [`said in ${round.posted}`])]),
    el("div", { class: "choices" }, choices),
  ];
  if (revealed) {
    const author = round.choices.find((c) => c.id === round.answer);
    const gained = round.gained[me];
    const line = gained ? `Right! +${gained}` : mine ? `It was ${author.name}.` : `Too slow! It was ${author.name}.`;
    parts.push(
      el("p", { class: "reveal-line" }, [
        line,
        " ",
        el("button", { class: "link", onclick: () => sdk.commands.openExternalLink({ url: round.link }) }, [
          "See the message",
        ]),
      ]),
    );
  }
  parts.push(players());
  return parts;
}

function done() {
  const ranked = [...view.players].sort((a, b) => b.score - a.score);
  const medals = ["🥇", "🥈", "🥉"];
  return [
    el("section", { class: "center" }, [
      el("h1", {}, ["Final scores"]),
      el(
        "ol",
        { class: "board" },
        ranked.map((p, i) =>
          el("li", {}, [
            el("span", { class: "place" }, [medals[i] ?? String(i + 1)]),
            avatar(p),
            el("span", { class: "name" }, [p.name]),
            el("span", { class: "score" }, [String(p.score)]),
          ]),
        ),
      ),
      el("button", { class: "primary", onclick: () => send({ type: "start" }) }, ["Play again"]),
    ]),
  ];
}

function players() {
  return el(
    "div",
    { class: "players" },
    view.players.map((p) => {
      const classes = ["player"];
      if (!p.connected) classes.push("away");
      if (p.answered) classes.push("answered");
      const score = view.phase === "lobby" ? [] : [el("strong", {}, [String(p.score)])];
      return el("div", { class: classes.join(" ") }, [avatar(p, true), p.name, ...score]);
    }),
  );
}

// An avatar picture, or the first letter of the name when there's none or it won't load.
function avatar(person, small = false) {
  const letter = () => el("span", { class: small ? "avatar small" : "avatar" }, [person.name.slice(0, 1).toUpperCase()]);
  if (!person.avatar) return letter();
  const img = el("img", { class: small ? "avatar small" : "avatar", src: person.avatar, alt: "" }, []);
  img.onerror = () => img.replaceWith(letter());
  return img;
}

function showMessage(text, isError = false) {
  app.replaceChildren(
    el("section", { class: "center" }, [
      el("h1", {}, ["Who said it", el("span", {}, ["?"])]),
      el("p", { class: isError ? "error" : "muted" }, [text]),
    ]),
  );
}

// Makes an element. Text goes in as text, never as HTML, so a message can't add markup.
function el(tag, attributes, children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attributes)) {
    if (key.startsWith("on")) node[key] = value;
    else if (key === "disabled") node.disabled = value;
    else node.setAttribute(key, value);
  }
  for (const child of children) {
    if (child !== null) node.append(child);
  }
  return node;
}

// How much of the question's time is left, in percent.
function timeLeft() {
  if (view.phase !== "question") return 0;
  return (100 * Math.max(0, deadline - performance.now())) / view.round.answer_ms;
}

// The countdown, redrawn a few times a second.
setInterval(() => {
  const bar = document.getElementById("bar");
  const seconds = document.getElementById("seconds");
  if (!bar || !view || !view.round) return;
  bar.style.width = `${timeLeft()}%`;
  const left = Math.max(0, deadline - performance.now());
  seconds.textContent = view.phase === "question" ? `${Math.ceil(left / 1000)}s` : "";
}, 250);
