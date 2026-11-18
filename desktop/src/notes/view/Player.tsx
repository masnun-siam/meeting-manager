import { type KeyboardEvent, useEffect, useRef, useState } from 'react';
import { type Note, noteOpen, recordingUrl } from '../../api';
import { PauseIcon, PlayIcon } from '../../icons';
import { attempt } from '../../recording/attempt';
import {
  formatClock,
  LOADING_RECORDING,
  RECORDING_DELETED,
  RECORDING_MISSING,
  recordingOpenProblem,
  UNPLAYABLE,
} from './format';
import styles from './Player.module.css';

/** What the note page may do with the player: #210 seeks it once `ready`, #211 pauses it first. */
export type PlayerHandle = { ready: boolean; seek: (seconds: number) => void; pause: () => void };

export const SPEEDS = [1, 1.25, 1.5, 2];
/** How far an arrow key moves the seek bar. */
export const STEP_S = 5;

type Props = {
  note: Pick<Note, 'id' | 'media' | 'recording_deleted' | 'recording_missing' | 'duration_s'>;
  say: (message: string | null) => void;
  /** Reloads the note; after a media error it tells a vanished recording from an unplayable one. */
  reload: () => Promise<unknown>;
  /** Called with the handle whenever `ready` changes, and with null once the player is gone. */
  onHandle: (handle: PlayerHandle | null) => void;
};

/**
 * The recording's player (FR-5, FR-8, FR-9, D3): sound only, for every recording type. Without a
 * recording (`media` null) it says why instead.
 */
export function Player({ note, ...props }: Props) {
  if (note.media) return <Audio id={note.id} knownLength={note.duration_s} {...props} />;
  const text = note.recording_deleted ? RECORDING_DELETED : note.recording_missing ? RECORDING_MISSING : null;
  return text ? <p className={styles.gone}>{text}</p> : null;
}

/** `checking`: a media error, waiting for the reload that says whether the file is still there. */
type Phase = 'loading' | 'ready' | 'checking' | 'unplayable';

function Audio({
  id,
  knownLength,
  say,
  reload,
  onHandle,
}: Omit<Props, 'note'> & { id: number; knownLength: number | null }) {
  const audio = useRef<HTMLAudioElement>(null);
  const group = useRef<HTMLFieldSetElement>(null);
  const openButton = useRef<HTMLButtonElement>(null);
  const gone = useRef(false);
  // Whether a control had focus when an error took the controls away: Open recording takes it.
  const hadFocus = useRef(false);
  const [phase, setPhase] = useState<Phase>('loading');
  const [duration, setDuration] = useState(0);
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  const ready = phase === 'ready';

  // Set here, not as a prop, so the cleanup that empties it on leaving (no sound plays on and no
  // request stays open) is undone by StrictMode's second mount.
  useEffect(() => {
    const element = audio.current;
    if (!element) return;
    gone.current = false;
    element.src = recordingUrl(id);
    return () => {
      gone.current = true;
      element.pause();
      element.removeAttribute('src');
      element.load();
    };
  }, [id]);

  useEffect(() => {
    onHandle({
      ready,
      seek: (seconds) => {
        const element = audio.current;
        if (!element || !ready) return;
        element.currentTime = Math.min(Math.max(0, seconds), length(element, knownLength));
      },
      pause: () => audio.current?.pause(),
    });
  }, [ready, knownLength, onHandle]);

  useEffect(() => () => onHandle(null), [onHandle]);

  const measure = () => {
    if (audio.current) setDuration(length(audio.current, knownLength));
  };

  const loaded = () => {
    measure();
    setPhase((now) => (now === 'loading' ? 'ready' : now));
  };

  useEffect(() => {
    if (phase === 'unplayable' && hadFocus.current) openButton.current?.focus();
  }, [phase]);

  const failed = async () => {
    // One check per failure: a repeated `error` event changes nothing.
    if (gone.current || phase === 'checking' || phase === 'unplayable') return;
    hadFocus.current = group.current?.contains(document.activeElement) ?? false;
    setPhase('checking');
    setPlaying(false);
    // A recording moved away mid-play errors too: the reload then gives `media: null`, and this
    // player is replaced by "Recording missing" before it can call the file unplayable.
    await reload();
    if (!gone.current) setPhase('unplayable');
  };

  const toggle = () => {
    const element = audio.current;
    if (!element) return;
    // A refused play() also fires the element's `error` event, which is handled there.
    if (element.paused) element.play().catch(() => {});
    else element.pause();
  };

  const seek = (seconds: number) => {
    const element = audio.current;
    if (!element) return;
    const at = Math.min(Math.max(0, seconds), duration);
    element.currentTime = at;
    setTime(at);
  };

  const onSeekKey = (e: KeyboardEvent) => {
    const to: Record<string, number> = {
      ArrowLeft: time - STEP_S,
      ArrowDown: time - STEP_S,
      ArrowRight: time + STEP_S,
      ArrowUp: time + STEP_S,
      Home: 0,
      End: duration,
    };
    if (!(e.key in to)) return;
    e.preventDefault();
    seek(to[e.key]);
  };

  const changeSpeed = (rate: number) => {
    setSpeed(rate);
    const element = audio.current;
    if (!element) return;
    // The default too, so a reloaded source keeps the chosen speed.
    element.defaultPlaybackRate = rate;
    element.playbackRate = rate;
  };

  const open = async () => {
    say(null);
    const result = await attempt(() => noteOpen({ id, what: 'recording' }));
    if (!result.ok) say(recordingOpenProblem(result.code, result.text));
  };

  const status = phase === 'loading' ? LOADING_RECORDING : phase === 'unplayable' ? UNPLAYABLE : null;
  return (
    <fieldset ref={group} aria-label="Recording player" className={styles.player}>
      {/* No `controls` (the app's own) and no `crossorigin`: the recording:// handler sends no CORS headers. */}
      {/* biome-ignore lint/a11y/useMediaCaption: the note's own recording, sound only; its Transcript tab is the text alternative (WCAG 1.2.1). */}
      <audio
        ref={audio}
        preload="metadata"
        onLoadedMetadata={loaded}
        onDurationChange={measure}
        onTimeUpdate={() => setTime(audio.current?.currentTime ?? 0)}
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => setPlaying(false)}
        onError={() => void failed()}
      />
      {/* Always mounted: screen readers skip a live region that appears already filled. */}
      <span role="status" className={styles.status}>
        {status}
      </span>
      {phase === 'unplayable' ? (
        <button ref={openButton} type="button" onClick={() => void open()}>
          Open recording
        </button>
      ) : (
        <>
          <button
            type="button"
            data-variant="plain"
            className={styles.play}
            aria-label={playing ? 'Pause' : 'Play'}
            disabled={!ready}
            onClick={toggle}
          >
            {playing ? <PauseIcon size={14} /> : <PlayIcon size={14} />}
          </button>
          <span className={styles.time}>{formatClock(time)}</span>
          <input
            type="range"
            aria-label="Seek"
            aria-valuetext={`${formatClock(time)} of ${formatClock(duration)}`}
            className={styles.seek}
            min={0}
            max={duration}
            step={1}
            value={Math.min(time, duration)}
            disabled={!ready}
            onChange={(e) => seek(Number(e.target.value))}
            onKeyDown={onSeekKey}
          />
          <span className={styles.time}>{formatClock(duration)}</span>
          <select
            aria-label="Speed"
            className={styles.speed}
            value={speed}
            onChange={(e) => changeSpeed(Number(e.target.value))}
          >
            {SPEEDS.map((rate) => (
              <option key={rate} value={rate}>
                {rate}×
              </option>
            ))}
          </select>
        </>
      )}
    </fieldset>
  );
}

/**
 * The file's length. A recording made by MediaRecorder (WebM) has no length in its header, so the
 * element reports Infinity; the length the store measured stands in for it.
 */
function length(element: HTMLMediaElement, known: number | null): number {
  if (Number.isFinite(element.duration)) return element.duration;
  return known ?? 0;
}
