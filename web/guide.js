// The user guide, in the app.
//
// It could have been a page on the project's website, and that is what the Help
// menu of most applications points at. Two things argue against it here. This
// app is a way of playing a game offline-ish on a laptop, and a Help menu that
// needs the network to answer is one that fails exactly when someone is trying
// to work out why nothing is loading. And the guide is about *this build* — the
// settings it has, the things it does and does not do — so a link to whatever is
// currently published would drift from the copy in front of the player.
//
// The text is data rather than markup for the same reason the settings panel's
// controls are: it is the specification, the tests read it, and adding a section
// takes no other change.

import * as diagnostics from './diagnostics.js';

/**
 * @typedef {{ heading: string, body: string[] }} Section
 * @type {Section[]}
 */
export const GUIDE = [
  {
    heading: 'Signing in',
    body: [
      'Use the same ArenaNet account you would use anywhere else. The login goes ' +
        'to ArenaNet, not to this app; nothing about your account is stored on this Mac ' +
        'beyond what the game itself writes.',
      'Characters, settings, hero builds and storage all live on ArenaNet’s servers. ' +
        'Nothing on this Mac holds them, which is why anything here can be deleted and ' +
        'rebuilt without losing a character.',
    ],
  },
  {
    heading: 'How the game loads',
    body: [
      'Guild Wars is a single 4.2 GB file. The first launch asks whether to stream it — ' +
        'fetching each area the first time you visit it, which starts playing in seconds — ' +
        'or to download all of it first, which takes a while once and then never touches ' +
        'the network for game data again.',
      'Either answer can be changed later under Game image in Settings, and a download ' +
        'started there continues in the background while you play. Streaming is not a ' +
        'lesser mode: it fetches ahead of where you are, and what it fetches it keeps.',
    ],
  },
  {
    heading: 'Double-click and right-click',
    body: [
      'The game has no double-click of its own — it is assembled from taps, and this app ' +
        'supplies them. That is what the Double-click setting does, and it has to be On ' +
        'for picking up items, equipping and most of the inventory to work. Touch only ' +
        'withholds mouse clicks from the game entirely, and Off removes the taps; on ' +
        'either of those, double-clicking does nothing at all.',
      'Right-click and right-drag to turn the camera work through pointer lock, which ' +
        'macOS grants only after you have clicked into the game window once. If the ' +
        'camera will not turn, click once in the game and try again.',
    ],
  },
  {
    heading: 'Settings',
    body: [
      'Press ⌘, at any time, including before the game has finished starting.',
      'Render scale is the one that costs frames. 2× draws one game pixel per display ' +
        'pixel on a Retina screen and is the sharpest; 1× is the fastest. It and ' +
        'Double-click are read when the app starts, so the panel offers to restart ' +
        'rather than pretending they took effect.',
      'Diagnostics overlay shows what the app is doing, as it does it. It is worth ' +
        'turning on only when something is wrong and worth turning off again afterwards.',
    ],
  },
  {
    heading: 'When something looks wrong',
    body: [
      'Turn the diagnostics overlay on and look at the last few lines. Almost everything ' +
        'this app does that can fail says so there, in a sentence rather than a code.',
      'Help → Report a Problem… writes a single file describing this Mac, your settings ' +
        'and the last few minutes of what the app recorded, and shows it to you in the ' +
        'Finder. That is the file to attach. Anything shaped like an email address is ' +
        'removed from it, and it is a plain text file you can read before sending it.',
      'For stutter and slow frames, press ⌘⇧M while it is happening — every time it ' +
        'happens — and then save the report. That marks the moment in the recording and ' +
        'measures the next ten seconds ten times more finely than usual. Pressing it ' +
        'afterwards does not work: it cannot go back and look more closely at a moment ' +
        'that has passed.',
      'A game that hitches while moving between areas on a slow connection is streaming, ' +
        'not breaking. Downloading in full removes it for good.',
      'Quitting from the menu, or with ⌘Q, lets the game write its files out first. ' +
        'Force-quitting does not.',
    ],
  },
  {
    heading: 'Enhancements',
    body: [
      'Two read-only extras in Settings. Game cursor is on by default; Target distance ' +
        'is off. Turning both off removes the optional tools; the separate build-template ' +
        'repair may still be active.',
      'Game cursor draws the game\'s own pointer as the Mac\'s pointer. The game normally ' +
        'paints its cursor into the picture, which means the cursor arrives with the frame ' +
        'and lags behind your hand whenever the frame rate drops. With this on, the pointer ' +
        'is the system\'s and moves at full speed, wearing the picture the game meant it ' +
        'to wear.',
      'Target distance shows how far away your current target is, and which of the game\'s ' +
        'range bands that falls in — Adjacent, Nearby, Area, Earshot, Spellcast, Spirit, ' +
        'Compass. It reads the number the game already has and shows it; it does not ' +
        'measure anything the game does not know.',
      'Both are read when the app starts, so the panel offers to restart rather than ' +
        'pretending they took effect. Both also need this app to recognise the inside of ' +
        'the client ArenaNet is shipping, the same recognition build templates need. On a ' +
        'client build this release has not been checked against, they stay off and the app ' +
        'says so in the diagnostics overlay.',
    ],
  },
  {
    heading: 'Build templates',
    body: [
      'Saving a build template needs this app to recognise the inside of the client ' +
        'ArenaNet is shipping, and ArenaNet changes it without warning. On a client build ' +
        'this release has not been checked against, the Save button in the template window ' +
        'does nothing and the app says so — once at the launch it becomes true, and ' +
        'whenever Settings is open. Everything else, including loading templates other ' +
        'people have posted, is unaffected.',
    ],
  },
  {
    heading: 'Disk space',
    body: [
      'Everything downloaded is a cache. Clear Game Data… in Settings deletes it at the ' +
        'next launch — it has to be a launch, because the running game is reading it — and ' +
        'the game fetches back only what it asks for. Nothing that belongs to your account ' +
        'is in there.',
    ],
  },
  {
    heading: 'Legal',
    body: [
      'gwnative is an independent, unofficial interoperability project for Guild Wars ' +
        'Reforged. It is not affiliated with, endorsed, sponsored, or approved by ' +
        'ArenaNet or NCSOFT. The official client and game data are downloaded from ' +
        'ArenaNet and are not covered by gwnative’s GPL licence.',
      '© ArenaNet LLC. All rights reserved. NCSOFT, ArenaNet, Guild Wars, Guild Wars 2, ' +
        'GW2, Heart of Thorns, Path of Fire, End of Dragons, Secrets of the Obscure, ' +
        'Janthir Wilds, Visions of Eternity, and all associated logos, designs, and ' +
        'composite marks are trademarks or registered trademarks of NCSOFT Corporation. ' +
        'All other trademarks are the property of their respective owners.',
    ],
  },
];

/**
 * Wire the guide to the document and return the opener.
 *
 * Built once, on install, rather than on each open: it is a few hundred words of
 * static text, and building it while the client is running would be a layout
 * pass in the middle of a frame for no reason.
 *
 * @param {{ log: (...args: unknown[]) => void }} deps
 * @returns {() => void} opens the guide
 */
export function installGuide({ log }) {
  const overlay = document.getElementById('guide');
  const body = document.getElementById('guide-body');
  const actions = document.getElementById('guide-actions');
  if (!overlay || !body || !actions) {
    log('[warn] guide: the guide is not in this page');
    return () => {};
  }

  for (const section of GUIDE) {
    const heading = document.createElement('h2');
    heading.textContent = section.heading;
    body.append(heading);
    for (const paragraph of section.body) {
      const text = document.createElement('p');
      text.textContent = paragraph;
      body.append(text);
    }
  }

  const close = () => {
    overlay.hidden = true;
    // The client stops hearing keys the moment something else takes focus, and
    // the guide took it. Same handover the settings panel makes.
    document.getElementById('canvas')?.focus();
  };

  const done = document.createElement('button');
  done.textContent = 'Done';
  done.classList.add('primary');
  done.addEventListener('click', close);
  actions.append(done);

  overlay.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.stopPropagation();
      close();
    }
  });

  return () => {
    // Back to the top: a guide reopened where it was last scrolled to looks like
    // it failed to open.
    body.scrollTop = 0;
    overlay.hidden = false;
    done.focus();
    diagnostics.count('gw.guide.opened');
  };
}
