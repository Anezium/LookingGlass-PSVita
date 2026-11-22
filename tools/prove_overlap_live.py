"""Step 3 proofs for lag one admitted in ordinary play (`sim_overlap_normal`).

live: host live play with no tape session. `LOOKING_GLASS_LIVE_EPOCH_TAPS` presses one key of
F13..F24 in every framework epoch; forced fences make repeated lag entries and drains. Every
epoch's tap must reach exactly one step, entry heads must latch the clock, and the same run with
`LOOKING_GLASS_LIVE_EPOCHS=off` must show the duplicated and lost taps the epochs prevent.
A second live run crosses listed and unlisted maps by natural exits and checks that only listed
visits dispatch and retain.

replay: the PotEars2 tape crossing the same maps by natural exits, OFF against the normal
layout, exact on every oracle channel, with dispatch and retention only on listed visits.

ui: host live play on PotEars2 with the normal layout and lag one admitted, driven through the UI
states that heads of a listed visit can now reach (review S34 F1). `LOOKING_GLASS_LIVE_KEYS` holds
keys at the framework capture, as physical keys arrive, so the real Start (Escape) path runs,
including the throttled-preview frozen frame before the menu. The run holds the skip keys at entry,
toggles pause, inventory and the chapter chooser, opens and closes the Start menu, dies
(`die` host action) and retries from the death screen with Enter, and mutes and unmutes. Each state
must be reached, no head in it may dispatch, lag must dispatch again after it, the death retry must
reinstall the listed map as a new visit, and the run must exit cleanly with its dispatch and
retention totals equal to the per-visit audit events.
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import prove_overlap_saves as saves  # noqa: E402

POOL = 12
HEAD = re.compile(r'^LIVE_EPOCH_HEAD frame=(\d+) epoch=(\d+) taps=([0-9a-f]+) clock=(\d+) latched=(true|false)$')
CLOCK = re.compile(r'^LIVE_EPOCH_CLOCK frame=(\d+) clock=(\d+) latched=(true|false)$')
LAYOUT = re.compile(r'^\[lg/sim-layout\] map=(\S+) visit=(\d+) active_skin_workers=(\d+)')
OVERLAP = re.compile(r'^\[lg/sim-overlap\] dispatched_frames=(\d+) retained_frames=(\d+)')
LISTED = ['potears2']
# Natural exits as mid-step exits (LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS), not the transition
# probe, whose diagnostic tail fence would stop retention.
EXIT_ORDER = ['potears3', 'potears2', 'wchess1', 'potears2']
LIVE_EXITS = ','.join(f'{f}:natural-exit={m}' for f, m in zip((150, 330, 520, 700), EXIT_ORDER))
REPLAY_EXITS = ','.join(f'{f}:natural-exit={m}' for f, m in zip((400, 800, 1200, 1600), EXIT_ORDER))


def normal_config(base):
    config = dict(base, sim_overlap=True, sim_overlap_normal=True, sim_overlap_maps=LISTED,
                  skin_workers=2)
    assert config['skin_overlap'] and config['audio_pcm_worker']
    assert config['audio_producer_affinity'] == 0x40000
    return config


def launch(binary, data, config, sim, work, args, env, timeout_s=600):
    if work.exists():
        shutil.rmtree(work)
    private = work / 'private'
    private.mkdir(parents=True)
    (private / 'vita_renderer.json').write_text(json.dumps(config, indent=2) + '\n')
    shutil.copy2(sim, private / 'vita_sim.json')
    full_env = {k: v for k, v in os.environ.items() if not k.startswith('LOOKING_GLASS_')}
    full_env.update(LANG='C', **env)
    started = time.time()
    proc = subprocess.run([str(binary), '--data', str(data), '--save-dir', str(work / 'saves'), *args],
                          cwd=work, env=full_env, capture_output=True, text=True, timeout=timeout_s)
    (work / 'stdout.log').write_text(proc.stdout)
    (work / 'stderr.log').write_text(proc.stderr)
    if proc.returncode != 0:
        raise RuntimeError(f'viewer exited {proc.returncode} in {work}\n{proc.stderr[-3000:]}')
    return proc.stdout.splitlines(), proc.stderr.splitlines(), round(time.time() - started, 1)


def visits(stdout, stderr):
    """Per installed visit: map, active skin workers, and its lag events."""
    layouts = [(m[1], int(m[2]), int(m[3])) for line in stderr if (m := LAYOUT.match(line))]
    events = defaultdict(Counter)
    for line in stdout:
        if line.startswith('SIM_EVENT '):
            row = json.loads(line.split(' ', 1)[1])
            if 'visit' in row:
                events[row['visit']][row['event']] += 1
    rows = [dict(map=name, visit=visit, active_skin_workers=workers,
                 dispatch=events[visit]['dispatch'], pending=events[visit]['pending'])
            for name, visit, workers in layouts]
    counts = [tuple(map(int, m.groups())) for line in stderr + stdout if (m := OVERLAP.match(line))]
    return rows, counts


def check_visits(rows, counts, label):
    assert rows, f'{label}: no layout installs'
    for row in rows:
        listed = row['map'] in LISTED
        assert row['active_skin_workers'] == (1 if listed else 2), (label, row)
        if listed:
            assert row['dispatch'] > 0 and row['pending'] > 0, (label, row)
        else:
            assert row['dispatch'] == 0 and row['pending'] == 0, (label, row)
    (dispatched, retained), = counts
    assert dispatched == sum(r['dispatch'] for r in rows), (label, counts, rows)
    assert retained == sum(r['pending'] for r in rows), (label, counts, rows)


def epochs(stdout):
    heads = [(int(m[1]), int(m[2]), int(m[3], 16), int(m[4]), m[5] == 'true')
             for line in stdout if (m := HEAD.match(line))]
    assert heads, 'no live epoch heads'
    delivered = Counter()
    for _, epoch, taps, _, _ in heads:
        for index in range(POOL):
            if taps >> index & 1:
                # The newest epoch at or before this capture with that tap.
                delivered[epoch - (epoch - index) % POOL] += 1
    first, last = heads[0][1], heads[-1][1]
    window = range(first, last + 1)
    duplicated = sorted(e for e in window if delivered[e] > 1)
    lost = sorted(e for e in window if delivered[e] == 0)
    latched = [i for i, head in enumerate(heads) if head[4]]
    # A latched head delivers no edge and runs its step on the previous step's clock.
    clocks = [(int(m[1]), int(m[2]), m[3] == 'true') for line in stdout if (m := CLOCK.match(line))]
    by_frame = {frame: (clock, held) for frame, clock, held in clocks}
    latched_frames = [heads[i][0] for i in latched]
    clocks_held = all(heads[i][2] == 0 for i in latched) and all(
        by_frame[f][1] and f - 1 in by_frame and by_frame[f][0] == by_frame[f - 1][0]
        for f in latched_frames)
    assert all(held == (frame in set(latched_frames)) for frame, _, held in clocks), 'latch mismatch'
    carried = sum(1 for _, _, taps, _, _ in heads if bin(taps).count('1') > 1)
    return dict(heads=len(heads), epochs=len(window), duplicated=duplicated, lost=lost,
                latched_heads=len(latched), latched_clock_and_no_edges=clocks_held,
                carried_heads=carried)


def run_live(args, out):
    base = json.loads(Path(args.config).read_text())
    config = normal_config(base)
    tap_env = dict(LOOKING_GLASS_LIVE_EPOCH_TAPS='1', LOOKING_GLASS_SIM_AUDIT='1',
                   LOOKING_GLASS_SIM_FENCE_EVERY='9')
    live_args = ['--map', 'potears2', '--new-game', '--frames', '600']
    report = {}
    for name, extra in (('epochs_on', {}), ('epochs_off', dict(LOOKING_GLASS_LIVE_EPOCHS='off'))):
        stdout, stderr, elapsed = launch(args.binary, args.data, config, args.sim, out / name,
                                         live_args, dict(tap_env, **extra))
        rows, counts = visits(stdout, stderr)
        check_visits(rows, counts, name)
        summary = epochs(stdout)
        summary.update(visits=rows, overlap=counts, elapsed_s=elapsed)
        report[name] = summary
        print(name, json.dumps({k: v for k, v in summary.items() if k != 'visits'}), flush=True)
    on, off = report['epochs_on'], report['epochs_off']
    assert not on['duplicated'] and not on['lost'], on
    assert on['latched_heads'] > 0 and on['latched_clock_and_no_edges'] and on['carried_heads'] > 0, on
    assert off['duplicated'] and off['lost'] and off['latched_heads'] == 0, off
    stdout, stderr, elapsed = launch(
        args.binary, args.data, config, args.sim, out / 'multimap_live',
        ['--map', 'potears2', '--new-game', '--frames', '900'],
        dict(tap_env, LOOKING_GLASS_SIM_FENCE_EVERY='13',
             LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS=LIVE_EXITS))
    rows, counts = visits(stdout, stderr)
    check_visits(rows, counts, 'multimap_live')
    assert [r['map'] for r in rows] == ['potears2', 'potears3', 'potears2', 'wchess1', 'potears2'], rows
    summary = epochs(stdout)
    assert not summary['duplicated'] and not summary['lost'], summary
    summary.update(visits=rows, overlap=counts, elapsed_s=elapsed)
    report['multimap_live'] = summary
    print('multimap_live', json.dumps(summary), flush=True)
    # A failed exit build rolls back to the outgoing visit and recommits its layout; a death
    # retry reinstalls the map as a new visit through the save-load path.
    # A menu New Game then installs the unlisted gvillage through the chapter/new-game path.
    stdout, stderr, elapsed = launch(
        args.binary, args.data, dict(config, story_film_bypass=True), args.sim,
        out / 'rollback_retry_live', ['--map', 'potears2', '--new-game', '--frames', '560'],
        dict(tap_env, LOOKING_GLASS_TRANSITION_FAIL_BUILD='1', LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS=(
            '120:natural-exit=potears3,200:menu-save,300:retry,480:new-game')))
    rows, counts = visits(stdout, stderr)
    assert [r['map'] for r in rows] == ['potears2', 'potears2', 'potears2', 'gvillage'], rows
    assert len({r['visit'] for r in rows}) == 4, rows
    fault = [i for i, line in enumerate(stderr) if 'host transition fault at npcs' in line]
    layout = [i for i, line in enumerate(stderr) if LAYOUT.match(line)]
    assert len(fault) == 1 and layout[0] < fault[0] < layout[1], 'rollback did not recommit the layout'
    assert any('kind=death-retry' in line for line in stderr), 'no death retry'
    check_visits(rows, counts, 'rollback_retry_live')
    # Ordinary play keeps writes: the live menu save lands in the isolated save directory.
    slot = json.loads((out / 'rollback_retry_live' / 'saves' / 'slot1.json').read_text())
    jpeg = bytes(slot['preview']['jpeg'])
    assert slot['payload']['current'].startswith('potears2') and jpeg[:2] == b'\xff\xd8', 'menu save'
    summary = epochs(stdout)
    assert not summary['duplicated'] and not summary['lost'], summary
    summary.update(visits=rows, overlap=counts, elapsed_s=elapsed, menu_save_jpeg_bytes=len(jpeg))
    report['rollback_retry_live'] = summary
    print('rollback_retry_live', json.dumps(summary), flush=True)
    return report


def run_replay(args, out):
    base = json.loads(Path(args.config).read_text())
    plan = dict(LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS=REPLAY_EXITS, LOOKING_GLASS_SIM_AUDIT='1')
    arms = {}
    for name, overrides in (('off', {}), ('lag', dict(sim_overlap=True, sim_overlap_normal=True,
                                                       sim_overlap_maps=LISTED, skin_workers=2))):
        arms[name] = saves.run_viewer(args.binary, args.data, args.config, args.sim,
                                      out / f'replay_{name}', tape_src=args.tape,
                                      overrides=overrides, extra_env=plan, timeout_s=900)
    saves.compare_channels(arms['off'], arms['lag'], 'multimap_replay')
    assert arms['off']['checks'] == arms['lag']['checks'], 'replay check lines differ'
    assert arms['off']['events'] == arms['lag']['events'], 'replay event lines differ'
    work = out / 'replay_lag'
    rows, counts = visits(arms['lag'].pop('stdout_lines'), arms['lag'].pop('stderr_lines'))
    check_visits(rows, counts, 'multimap_replay')
    assert [r['map'] for r in rows] == ['potears2', 'potears3', 'potears2', 'wchess1', 'potears2'], rows
    summary = dict(frames=arms['lag']['frames'], visits=rows, overlap=counts,
                   checks=len(arms['lag']['checks']), events=len(arms['lag']['events']),
                   channels_exact=True)
    print('multimap_replay', json.dumps(summary), flush=True)
    for name in arms:
        shutil.rmtree(out / f'replay_{name}')
    del work
    return dict(multimap_replay=summary)


def sim_events(arm):
    return [json.loads(line.split(' ', 1)[1]) for line in arm['stdout_lines']
            if line.startswith('SIM_EVENT ')]


def run_controls(args, out):
    """I6 controls on a PotEars2 tape prefix with the normal layout admitted by the replay."""
    tape = out / 'potears2-400.lgtape'
    saves.slice_tape(args.tape, tape, 400)
    lag = dict(sim_overlap=True, sim_overlap_normal=True, sim_overlap_maps=LISTED, skin_workers=2)

    def arm(name, overrides, env, timeout_s=300):
        work = out / name
        try:
            result = saves.run_viewer(args.binary, args.data, args.config, args.sim, work,
                                      tape_src=tape, overrides=overrides,
                                      extra_env=dict(LOOKING_GLASS_SIM_AUDIT='1', **env),
                                      timeout_s=timeout_s)
        except subprocess.TimeoutExpired as timeout:
            text = timeout.stdout.decode() if isinstance(timeout.stdout, bytes) else (timeout.stdout or '')
            return dict(timed_out=True, stdout_lines=text.splitlines())
        except RuntimeError as error:
            return dict(failed=str(error))
        shutil.rmtree(work)
        return result

    off = arm('off', {}, {})
    reference = arm('lag', lag, {})
    saves.compare_channels(off, reference, 'controls:lag')
    events = sim_events(reference)
    pending = {e['frame'] for e in events if e['event'] == 'pending'}
    dispatch = {e['frame'] for e in events if e['event'] == 'dispatch'}
    injection = min(f for f in pending if f >= 100 and f - 1 in pending and f + 1 in dispatch)
    rows = dict(reference=dict(dispatch=len(dispatch), pending=len(pending), injection=injection))
    orders = [('rendezvous', dict(LOOKING_GLASS_SIM_LAG_ORDER='rendezvous'))] + [
        (f'{order}-{delay}', dict(LOOKING_GLASS_SIM_LAG_ORDER=order, LOOKING_GLASS_SIM_LAG_DELAY_US=str(delay)))
        for order in ('owner-first', 'worker-first') for delay in (0, 2000, 5000, 12000)]
    for tag, env in orders:
        result = arm(tag, lag, env)
        saves.compare_channels(off, result, f'controls:{tag}')
        ev = sim_events(result)
        marks = {(e['event'], e['frame']): i for i, e in enumerate(ev) if 'frame' in e}
        pairs = [e['frame'] for e in ev if e['event'] == 'owner_inner']
        assert injection in pairs, (tag, injection)
        crossing = 0
        for f in pairs:
            owner = marks['owner_inner', f], marks['owner_inner_end', f]
            worker = marks['worker_inner', f + 1], marks['worker_inner_end', f + 1]
            if tag.startswith('owner-first'):
                assert owner[1] < worker[0], (tag, f)
            elif tag.startswith('worker-first'):
                assert worker[1] < owner[0], (tag, f)
            crossing += owner[0] < worker[1] and worker[0] < owner[1]
        if tag == 'rendezvous':
            assert crossing > 0, tag
        rows[tag] = dict(pairs=len(pairs), intersecting=crossing)
        print('controls', tag, rows[tag], flush=True)
    serial = arm('serial-negative', lag, dict(LOOKING_GLASS_SIM_LAG_SERIAL='1'))
    saves.compare_channels(off, serial, 'controls:serial-negative')
    rows['serial-negative'] = dict(retained=serial['retained_frames'])
    stalled = arm('rendezvous-negative', lag, dict(LOOKING_GLASS_SIM_LAG_SERIAL='1',
                                                   LOOKING_GLASS_SIM_LAG_ORDER='rendezvous'), timeout_s=40)
    assert stalled.get('timed_out'), 'serial rendezvous must stall'
    stall = [json.loads(line.split(' ', 1)[1]) for line in stalled['stdout_lines'] if line.startswith('SIM_EVENT ')]
    last = [e['frame'] for e in stall if e['event'] == 'dispatch']
    assert last and last[-1] - 1 in {e['frame'] for e in stall if e['event'] == 'pending'}, 'no real dispatch'
    assert not any(e['event'] == 'owner_inner' for e in stall)
    rows['rendezvous-negative'] = dict(stalled_after_dispatch=last[-1])
    messages = dict(output='stale sim output', state='stale sim state', transport='stale sim transport',
                    present='stale present', seal='stale worker seal')
    for kind, message in messages.items():
        frame = injection if kind == 'seal' else injection + 1
        result = arm(f'stamp-{kind}', lag, dict(LOOKING_GLASS_SIM_WRONG_STAMP=f'{kind}:{frame}'))
        assert f'{message} stamp' in result.get('failed', ''), (kind, result.keys())
        rows[f'stamp-{kind}'] = dict(assertion=f'{message} stamp', frame=frame)
    wrong = arm('wrong-association', lag, dict(LOOKING_GLASS_SIM_LAG_WRONG_ASSOCIATION=str(injection)))
    changed = [k for k in ('images', 'presentation') if off[k] != wrong[k]]
    assert changed == ['images', 'presentation'], changed
    rows['wrong-association'] = dict(changed=changed)
    for n in (1, 2, 7):
        result = arm(f'fence-{n}', lag, dict(LOOKING_GLASS_SIM_FENCE_EVERY=str(n)))
        saves.compare_channels(off, result, f'controls:fence-{n}')
        fenced = [e['frame'] for e in sim_events(result) if e['event'] == 'dispatch']
        assert not any(f % n == 0 for f in fenced) and (not fenced) == (n == 1), (n, len(fenced))
        rows[f'fence-{n}'] = dict(dispatch=len(fenced), retained=result['retained_frames'])
    checkpoint = dict(LOOKING_GLASS_SIM_FORCE_CHECKPOINT=str(injection))
    off_checkpoint = arm('off-checkpoint', {}, checkpoint)
    mid = arm('mid-step', lag, checkpoint)
    saves.compare_channels(off_checkpoint, mid, 'controls:mid-step')
    reasons = {json.loads(l.split(' ', 1)[1])['frame']: json.loads(l.split(' ', 1)[1])['reason']
               for l in mid['stdout_lines'] if l.startswith('SIM_W0_FRAME ')}
    assert reasons[injection] == 'mid_step_checkpoint', reasons.get(injection)
    rows['mid-step'] = dict(frame=injection, reason=reasons[injection])
    print('controls', json.dumps(rows), flush=True)
    return dict(controls=rows)


UI_LINE = re.compile(r'^LIVE_UI frame=(\d+) t=([0-9.]+) map=(\S+) paused=(\w+) menu=(\w+) inventory=(\w+) '
                     r'escape=(\w+) alive=(\w+) scripted=(\w+) muted=(\w+)$')
UI_STATES = ('paused', 'inventory', 'menu', 'escape', 'dead', 'muted')
# Seconds after the first live capture. Enter and E (Cross and Triangle on the Vita) are held as
# skip keys at entry; Escape is Start, Tab is Select, I is the inventory, P pauses, M mutes.
UI_KEYS = ','.join(['0:Enter:3', '3.5:E:1.5', '6:P:0.1', '8:P:0.1', '10:I:0.1', '12:I:0.1',
                    '14:Tab:0.1', '16:Tab:0.1', '18:Escape:0.1', '21:Escape:0.1']
                   + [f'{t}:Enter:0.1' for t in range(24, 35)] + ['37:M:0.1', '40:M:0.1'])
UI_DIE_FRAME = 2600


def run_ui(args, out):
    base = json.loads(Path(args.config).read_text())
    config = normal_config(base)
    assert config['save_preview'] == 'throttled', 'the frozen preview frame needs save_preview throttled'
    stdout, stderr, elapsed = launch(
        args.binary, args.data, config, args.sim, out / 'ui_states_live',
        ['--map', 'potears2', '--new-game', '--frames', '5200'],
        dict(LOOKING_GLASS_SIM_AUDIT='1', LOOKING_GLASS_LIVE_KEYS=UI_KEYS,
             LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS=f'{UI_DIE_FRAME}:die'), timeout_s=900)
    assert not any('panicked' in line for line in stdout + stderr), 'panic in the UI-state run'
    heads = []
    for line in stdout:
        if (m := UI_LINE.match(line)):
            f = [m[1], m[2], m[3], *m.groups()[3:]]
            flags = dict(zip(('paused', 'menu', 'inventory', 'escape', 'alive', 'scripted', 'muted'),
                             (v == 'true' for v in f[3:])))
            flags['dead'] = not flags.pop('alive')
            heads.append(dict(frame=int(f[0]), t=float(f[1]), map=f[2], **flags))
    assert heads, 'no LIVE_UI heads'
    events = [json.loads(line.split(' ', 1)[1]) for line in stdout if line.startswith('SIM_EVENT ')]
    dispatch = sorted({e['frame'] for e in events if e['event'] == 'dispatch'})
    pending = sorted({e['frame'] for e in events if e['event'] == 'pending'})
    last_head = {h['frame']: h for h in heads}
    states = {}
    for name in UI_STATES:
        frames = [h['frame'] for h in heads if h[name]]
        assert frames, f'state {name} was never reached'
        first, last = frames[0], frames[-1]
        # A modal head runs again under the same frame number after the modal closes; the frame's
        # dispatch belongs to its last head line.
        in_state = {f for f, h in last_head.items() if h[name]}
        inside = [f for f in dispatch if f in in_state]
        kept = [f for f in pending if f in in_state]
        after = [f for f in pending if last < f <= last + 600]
        states[name] = dict(first_frame=first, last_frame=last, heads=len(frames),
                            first_t=next(h['t'] for h in heads if h[name]),
                            dispatched_in_state=len(inside), retained_in_state=len(kept),
                            retained_before=len([f for f in pending if first - 240 <= f < first]),
                            retained_after=len(after))
        if name != 'muted':
            # A paused, menu, inventory, Start or death head is not gameplay: it never dispatches.
            assert not inside, (name, inside[:5])
        assert after, f'lag did not retain again after {name}'
    # The Start press head renders one frozen serial frame (throttled preview) before the menu.
    frozen = states['escape']['first_frame'] - 1
    states['escape']['frozen_frame'] = frozen
    assert frozen not in dispatch and any(e['event'] == 'present' and e['frame'] == frozen
                                          and e['generation'] == 0 for e in events), 'no frozen serial frame'
    order = [states[n]['first_frame'] for n in ('paused', 'inventory', 'menu', 'escape', 'dead', 'muted')]
    assert order == sorted(order), states
    assert states['dead']['first_frame'] >= UI_DIE_FRAME
    retry = [line for line in stderr if '"kind":"death-retry"' in line and '"event":"end"' in line]
    assert len(retry) == 1 and '"success":true' in retry[0], 'no death retry from the death screen'
    rows, counts = visits(stdout, stderr)
    check_visits(rows, counts, 'ui_states_live')
    assert [r['map'] for r in rows] == ['potears2', 'potears2'] and rows[0]['visit'] != rows[1]['visit'], rows
    skip_heads = [h for h in heads if h['t'] - heads[0]['t'] < 5.0]
    summary = dict(states=states, visits=rows, overlap=counts, heads=len(heads),
                   dispatch=len(dispatch), pending=len(pending), scripted_heads=sum(h['scripted'] for h in heads),
                   skip_held_heads=len(skip_heads), elapsed_s=elapsed, keys=UI_KEYS, die_frame=UI_DIE_FRAME)
    print('ui_states_live', json.dumps(summary), flush=True)
    return dict(ui_states_live=summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('live', 'replay', 'controls', 'ui'))
    for name in ('binary', 'data', 'config', 'sim', 'out'):
        parser.add_argument(f'--{name}', required=True)
    parser.add_argument('--tape')
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    report = dict(live=run_live, replay=run_replay, controls=run_controls, ui=run_ui)[args.mode](args, out)
    report.update(binary=str(args.binary),
                  binary_sha256=hashlib.sha256(Path(args.binary).read_bytes()).hexdigest())
    (out / f'{args.mode}-proof.json').write_text(json.dumps(report, indent=2) + '\n')
    print('PASS', args.mode, flush=True)


if __name__ == '__main__':
    main()
