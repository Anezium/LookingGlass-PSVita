"""Step 4 save-parity and reload proof harness for lag-one sim overlap.

Compares OFF against lag-one overlap (existing replay admission) across all
writable save paths with isolated --save-dir (LOOKING_GLASS_REPLAY_WRITES=1)
and a pinned saved_at timestamp (LOOKING_GLASS_SAVE_FIXED_AT):
  - discard control (without LOOKING_GLASS_REPLAY_WRITES)
  - F5 quick save (first + overwrite -> .previous + embedded JPEG)
  - menu save (first + overwrite -> slot1 + .previous + embedded JPEG)
  - new-game preserve (auto + .previous + embedded JPEG)
  - arrival autosave via natural exit (auto + .previous + embedded JPEG)
  - quit autosave (throttled clean capture and immediate lag-quit)
  - forced finale checkpoint (writer off, writer on, delayed success, delayed failure)
  - reload verification (next-step digest, HUD/retry, and audio)
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

F5_BIT = 73
F5_BINDING_ROW = 20
FIXED_SAVED_AT = '1795031414000'
DROP_COUNTERS = (
    'dynamic_colliders_tested', 'dynamic_colliders_rejected',
    'skin_output_reused_vertices', 'world_alpha_parameter_once_hits',
    'vita_actor_discard_free_draws', 'vita_actor_cutout_discard_free_draws',
    'vita_fx_uniform_bytes', 'vita_fx_pipeline_depth_sets_skipped',
    'vita_fx_state_applications', 'vita_fx_uniform_sets_skipped',
    'vita_fx_add_lean_draws', 'vita_fx_add_fog_free_draws',
    'vita_fx_add_fog_volume_scans', 'vita_fx_unlit_exact_draws',
    'vita_sky_live_draws', 'vita_sky_live_triangles',
    'vita_sky_live_face_draws', 'vita_sky_live_target_draws',
    'vita_sky_live_lite_draws', 'vita_sky_live_upload_bytes',
)


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def digest_json(obj):
    return hashlib.sha256(
        json.dumps(obj, sort_keys=True, separators=(',', ':')).encode()
    ).hexdigest()


def normalize_frame(row):
    row = json.loads(json.dumps(row))
    for key in ('cpu_ms', 'spans_ms'):
        row.pop(key, None)
    row['counters'] = {
        k: v for k, v in row.get('counters', {}).items()
        if not k.endswith(('_us', '_ns', '_ms'))
        and not k.startswith(('actor_mem_', 'actor_publication_mem_'))
        and k not in DROP_COUNTERS
    }
    for metrics in (row.get('render') or {}).values():
        for key in list(metrics):
            if key.endswith('_ns'):
                metrics.pop(key)
    for packet in row.get('submitted_packets', []):
        packet.pop('gpu_slot', None)
    return row


def slice_tape(src, dst, max_frames, kp_frames=None, quit_frame=None, bind_f5=False):
    kp_frames = kp_frames or {}
    out = []
    frame_idx = 0
    checks = 0
    events = 0
    for line in Path(src).read_text(encoding='utf-8').splitlines():
        if line.startswith(('LGTAPE ', 'S ')):
            out.append(line)
        elif line.startswith('H '):
            if bind_f5:
                hdr = json.loads(line[2:])
                hdr['preferences']['bindings'][F5_BINDING_ROW] = 'F5'
                out.append('H ' + json.dumps(hdr, separators=(',', ':')))
            else:
                out.append(line)
        elif line.startswith('C '):
            step = int(line.split(' ', 2)[1])
            if step < max_frames:
                out.append(line)
                checks += 1
        elif line.startswith('E '):
            step = int(line.split(' ', 2)[1])
            if step < max_frames:
                out.append(line)
                events += 1
        elif line.startswith('F '):
            if frame_idx >= max_frames:
                continue
            tokens = line.split(' ')
            if frame_idx in kp_frames:
                mask = kp_frames[frame_idx]
                updated = False
                for i, tok in enumerate(tokens[2:], start=2):
                    if tok.startswith('kp='):
                        cur = int(tok.split('=', 1)[1], 16)
                        tokens[i] = f'kp={cur | mask:x}'
                        updated = True
                        break
                if not updated:
                    tokens.append(f'kp={mask:x}')
            if quit_frame is not None and frame_idx == quit_frame:
                if 'quit' not in tokens[2:]:
                    tokens.append('quit')
            out.append(' '.join(tokens))
            frame_idx += 1
    assert frame_idx == max_frames, f'expected {max_frames} frames, got {frame_idx}'
    end = json.dumps(
        dict(checks=checks, events=events, reason='tape-end', seconds=round(frame_idx / 30.0, 3), steps=frame_idx),
        sort_keys=True, separators=(',', ':'),
    )
    out.append(f'END {end}')
    Path(dst).write_text('\n'.join(out) + '\n', encoding='utf-8')


def run_viewer(binary, data_dir, config_path, sim_path, work_dir, tape_src=None,
               overrides=None, extra_env=None, seed_files=None, replay_record=None,
               replay_start=None, stop_after=0, timeout_s=300):
    if work_dir.exists():
        shutil.rmtree(work_dir)
    private = work_dir / 'private'
    private.mkdir(parents=True)
    saves = work_dir / 'isolated-saves'
    if seed_files:
        saves.mkdir(parents=True)
        for rel, content in seed_files.items():
            target = saves / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
    config = json.loads(Path(config_path).read_text(encoding='utf-8'))
    config.update(
        sky_live=False,
        audio_music_thread=False,
        audio_log_thread=False,
        audio_stats_log=False,
        log_frame_time_every=0,
        spike_log_ms=0.0,
        spike_log_first_frame=0,
        spike_log_last_frame=0,
        replay_play='',
        replay_record='',
        replay_start='',
    )
    if tape_src is not None:
        tape_name = Path(tape_src).name
        shutil.copy2(tape_src, private / tape_name)
        config['replay_play'] = tape_name
    if replay_record is not None:
        config['replay_record'] = replay_record
        config['replay_start'] = replay_start
        config['replay_check_every'] = 1
        config['replay_state_every'] = 1
        config['replay_stop_after'] = stop_after
    if overrides:
        config.update(overrides)
    (private / 'vita_renderer.json').write_text(json.dumps(config, indent=2) + '\n', encoding='utf-8')
    shutil.copy2(sim_path, private / 'vita_sim.json')
    env = dict(os.environ)
    env.update(
        LANG='C',
        LOOKING_GLASS_SIM_MAPS_PROBE='1',
        LOOKING_GLASS_AUDIO_PROOF='1',
        LOOKING_GLASS_SAH_SILENT_AUDIO='1',
        LOOKING_GLASS_REPLAY_CROSS='1',
        LOOKING_GLASS_VITA_CONTROLS='1',
        LOOKING_GLASS_SKIN_PROOF='1',
        LOOKING_GLASS_WORLD_PACKETS='1',
        LOOKING_GLASS_PROBE_KEEP_WINDOW='1',
        LOOKING_GLASS_SHOT_FRAMES='',
        LOOKING_GLASS_MOVER_REPLAY='1',
        LOOKING_GLASS_SNAPSHOT_FENCE_CENSUS='1',
    )
    for k in ('LOOKING_GLASS_REPLAY_WRITES', 'LOOKING_GLASS_SAVE_FIXED_AT',
              'LOOKING_GLASS_SIM_FORCE_CHECKPOINT', 'LOOKING_GLASS_SIM_LAG_QUIT',
              'LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS', 'LOOKING_GLASS_CHECKPOINT_WRITER_TEST',
              'LOOKING_GLASS_TRANSITION_DESTINATION', 'LOOKING_GLASS_TRANSITION_RESUME', 'LOOKING_GLASS_REPLAY_SYNTH'):
        env.pop(k, None)
    if extra_env:
        env.update(extra_env)

    signatures, images, presentation, mixes = [], [], [], []
    decodes, census, checks, events, notices = {}, [], [], [], []
    retained_frames = 0
    dispatched_frames = 0
    retry_restored = 0
    started = time.time()
    proc = subprocess.run(
        [str(binary), '--data', str(data_dir), '--save-dir', str(saves)],
        cwd=work_dir, env=env, capture_output=True, text=True, timeout=timeout_s,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f'viewer exited {proc.returncode} in {work_dir}\n'
            f'--- stderr ---\n{proc.stderr[-2000:]}\n--- stdout ---\n{proc.stdout[-2000:]}'
        )
    for line in proc.stdout.splitlines():
        if line.startswith('SIM_MAPS_FRAME '):
            raw = json.loads(line.split(' ', 1)[1])
            signatures.append([raw['frame'], digest_json(normalize_frame(raw))])
        elif line.startswith('SKIN_EXACT_IMAGE '):
            images.append(json.loads(line.split(' ', 1)[1]))
        elif line.startswith('SKIN_EXACT_PRESENTATION '):
            presentation.append(json.loads(line.split(' ', 1)[1]))
        elif line.startswith('AUDIO_MIX '):
            row = dict(re.findall(r'(\w+)=(\S+)', line))
            mixes.append([int(row['frame']), int(row['samples']), row['sha256']])
        elif line.startswith('AUDIO_DECODE '):
            row = dict(re.findall(r'(\w+)=(\S+)', line))
            decodes.setdefault(row['path'], set()).add((int(row['samples']), row['sha256']))
        elif line.startswith('SNAPSHOT_FENCE_CENSUS '):
            row = json.loads(line.split(' ', 1)[1])
            census.append(dict(frame=row['frame'], w0_failed=row.get('w0_failed', []),
                               blockers=sorted(set(row.get('blockers', [])))))
        elif line.startswith('[lg/sim-overlap] '):
            row = dict(re.findall(r'(\w+)=(\d+)', line))
            dispatched_frames = int(row.get('dispatched_frames', 0))
            retained_frames = int(row.get('retained_frames', 0))
        elif line.startswith('[lg/replay] check '):
            checks.append(re.sub(r'\s+cost_us=\d+', '', line.strip()))
        elif '[lg/replay] event ' in line:
            events.append(line.strip())
        elif line.startswith('Death retry: restored'):
            retry_restored += 1
        elif 'Checkpoint save failed:' in line or 'Saved ' in line:
            notices.append(line.strip())
    for line in proc.stderr.splitlines():
        if 'Checkpoint save failed:' in line or 'Save preview unavailable:' in line:
            notices.append(line.strip())

    # Inspect all files written in saves directory.
    files = {}
    for p in (sorted(saves.rglob('*')) if saves.exists() else ()):
        if p.is_file():
            rel = p.relative_to(saves).as_posix()
            raw = p.read_bytes()
            info = dict(bytes=len(raw), sha256=sha256_bytes(raw), raw=raw)
            if rel.endswith('.json'):
                env_json = json.loads(raw.decode('utf-8'))
                preview = env_json.get('preview')
                jpeg_bytes = None
                if isinstance(preview, dict) and isinstance(preview.get('jpeg'), list):
                    jpeg_bytes = bytes(preview['jpeg'])
                info['saved_at'] = env_json.get('saved_at')
                info['current'] = (env_json.get('payload') or {}).get('current')
                info['jpeg_len'] = len(jpeg_bytes) if jpeg_bytes is not None else 0
                info['jpeg_valid'] = (
                    jpeg_bytes is not None
                    and len(jpeg_bytes) > 100
                    and jpeg_bytes[:2] == b'\xff\xd8'
                    and jpeg_bytes[-2:] == b'\xff\xd9'
                )
                info['jpeg_sha256'] = sha256_bytes(jpeg_bytes) if jpeg_bytes is not None else None
            files[rel] = info

    recorded_checks = []
    if replay_record is not None:
        tape_out = private / replay_record
        if tape_out.is_file():
            for tline in tape_out.read_text(encoding='utf-8').splitlines():
                if tline.startswith('C '):
                    recorded_checks.append(tline)

    return dict(
        frames=len(signatures),
        signatures=signatures,
        images=images,
        presentation=presentation,
        audio_mix=mixes,
        audio_decode={k: sorted(v) for k, v in decodes.items()},
        census=census,
        checks=checks,
        recorded_checks=recorded_checks,
        events=events,
        notices=notices,
        retry_restored=retry_restored,
        dispatched_frames=dispatched_frames,
        retained_frames=retained_frames,
        files=files,
        elapsed_s=round(time.time() - started, 2),
        stdout_lines=proc.stdout.splitlines(),
        stderr_lines=proc.stderr.splitlines(),
    )


def compare_channels(off, lag, label):
    diffs = {}
    for key in ('frames', 'signatures', 'images', 'presentation', 'audio_mix', 'audio_decode'):
        if off[key] != lag[key]:
            if isinstance(off[key], list) and isinstance(lag[key], list):
                first = None
                for idx, (a, b) in enumerate(zip(off[key], lag[key])):
                    if a != b:
                        first = dict(index=idx, off=a, lag=b)
                        break
                diffs[key] = first or dict(len=[len(off[key]), len(lag[key])])
            else:
                diffs[key] = dict(off_keys=len(off[key]), lag_keys=len(lag[key]))
    if diffs:
        raise AssertionError(f'{label}: channel mismatch: {json.dumps(diffs)[:800]}')


def compare_files(off, lag, label, expected_files, expect_jpeg=True, fixed_saved_at_files=None):
    off_names = sorted(off['files'])
    lag_names = sorted(lag['files'])
    if off_names != lag_names:
        raise AssertionError(f'{label}: file set differs OFF={off_names} LAG={lag_names}')
    for rel in expected_files:
        if rel not in off['files']:
            raise AssertionError(f'{label}: missing expected file {rel} (got {off_names})')
    for rel in off_names:
        if rel.endswith('.tmp'):
            raise AssertionError(f'{label}: leftover temp file {rel}')
        a = off['files'][rel]
        b = lag['files'][rel]
        if a['sha256'] != b['sha256'] or a['bytes'] != b['bytes']:
            raise AssertionError(
                f'{label}: byte mismatch on {rel}: OFF={a["sha256"][:12]}({a["bytes"]}B) '
                f'LAG={b["sha256"][:12]}({b["bytes"]}B)'
            )
        if expect_jpeg and rel.endswith('.json'):
            if not a['jpeg_valid']:
                raise AssertionError(f'{label}: invalid or missing embedded JPEG in {rel}')
            if a['jpeg_sha256'] != b['jpeg_sha256']:
                raise AssertionError(f'{label}: JPEG sha mismatch in {rel}')
        if fixed_saved_at_files and rel in fixed_saved_at_files:
            if a['saved_at'] != int(FIXED_SAVED_AT):
                raise AssertionError(
                    f'{label}: {rel} saved_at={a["saved_at"]} != {FIXED_SAVED_AT}'
                )
    return {
        rel: dict(
            bytes=off['files'][rel]['bytes'],
            sha256=off['files'][rel]['sha256'],
            current=off['files'][rel].get('current'),
            saved_at=off['files'][rel].get('saved_at'),
            jpeg_bytes=off['files'][rel].get('jpeg_len', 0),
            jpeg_valid=off['files'][rel].get('jpeg_valid', False),
            jpeg_sha256=off['files'][rel].get('jpeg_sha256'),
        )
        for rel in off_names
    }


def verify_reload(binary, data_dir, config_path, sim_path, out_root, case_name,
                  slot_name, off_bytes, lag_bytes, lag_map='potears2'):
    assert off_bytes == lag_bytes, f'{case_name}: reload source bytes differ'
    filename = f'{slot_name}.json'
    reload_env = dict(
        LOOKING_GLASS_REPLAY_SYNTH='1',
        LOOKING_GLASS_SAVE_FIXED_AT=FIXED_SAVED_AT,
        LOOKING_GLASS_TRANSITION_RESUME='1',
    )
    rel_off = run_viewer(
        binary, data_dir, config_path, sim_path, out_root / f'{case_name}_reload_off',
        seed_files={filename: off_bytes},
        replay_record='reload.lgtape',
        replay_start=f'save:{slot_name}',
        stop_after=10,
        extra_env=reload_env,
    )
    rel_lag = run_viewer(
        binary, data_dir, config_path, sim_path, out_root / f'{case_name}_reload_lag',
        seed_files={filename: lag_bytes},
        replay_record='reload.lgtape',
        replay_start=f'save:{slot_name}',
        stop_after=10,
        extra_env=reload_env,
    )
    compare_channels(rel_off, rel_lag, f'{case_name}:reload(OFF-save vs LAG-save)')
    assert len(rel_off['recorded_checks']) >= 10, (
        f'{case_name}: expected >=10 reload checks, got {len(rel_off["recorded_checks"])}'
    )
    assert rel_off['recorded_checks'] == rel_lag['recorded_checks'], (
        f'{case_name}: reload next-step check digests differ'
    )
    assert rel_off['recorded_checks'][0].split()[2] != rel_off['recorded_checks'][1].split()[2] or (
        rel_off['recorded_checks'][0].split()[2] != rel_off['recorded_checks'][5].split()[2]
    ), f'{case_name}: reload did not advance simulation (still paused?)'
    # Also replay the recorded reload tape under lag 1 vs OFF when on the overlap map.
    replayed_lag_retained = 0
    if lag_map and rel_off['recorded_checks'] and f' {lag_map} ' in rel_off['recorded_checks'][0]:
        tape = out_root / f'{case_name}_reload_off' / 'private' / 'reload.lgtape'
        play_off = run_viewer(
            binary, data_dir, config_path, sim_path, out_root / f'{case_name}_replay_off',
            tape_src=tape,
            extra_env=dict(LOOKING_GLASS_TRANSITION_RESUME='1'),
        )
        play_lag = run_viewer(
            binary, data_dir, config_path, sim_path, out_root / f'{case_name}_replay_lag',
            tape_src=tape,
            overrides=dict(skin_workers=1, sim_overlap=True, sim_overlap_maps=[lag_map]),
            extra_env=dict(LOOKING_GLASS_TRANSITION_RESUME='1'),
        )
        compare_channels(play_off, play_lag, f'{case_name}:reload-replay(OFF vs LAG1)')
        replayed_lag_retained = play_lag['retained_frames']
        assert replayed_lag_retained > 0, f'{case_name}: reload replay retained 0 frames'
        shutil.rmtree(out_root / f'{case_name}_replay_off')
        shutil.rmtree(out_root / f'{case_name}_replay_lag')
    result = dict(
        slot=slot_name,
        steps=len(rel_off['recorded_checks']),
        first_check=rel_off['recorded_checks'][0],
        next_check=rel_off['recorded_checks'][1],
        channels_exact=True,
        audio_mix_frames=len(rel_off['audio_mix']),
        reload_replay_retained=replayed_lag_retained,
    )
    shutil.rmtree(out_root / f'{case_name}_reload_off')
    shutil.rmtree(out_root / f'{case_name}_reload_lag')
    return result


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', required=True)
    ap.add_argument('--data', required=True)
    ap.add_argument('--config', required=True)
    ap.add_argument('--sim', required=True)
    ap.add_argument('--tape', required=True, help='Path to a2-04.lgtape (potears2)')
    ap.add_argument('--out', required=True)
    args = ap.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    tapes_dir = out / 'tapes'
    tapes_dir.mkdir(exist_ok=True)

    tape_f5 = tapes_dir / 'potears2_f5.lgtape'
    slice_tape(args.tape, tape_f5, 80, kp_frames={30: 1 << F5_BIT, 60: 1 << F5_BIT}, bind_f5=True)
    tape_base80 = tapes_dir / 'potears2_80.lgtape'
    slice_tape(args.tape, tape_base80, 80)
    tape_base60 = tapes_dir / 'potears2_60.lgtape'
    slice_tape(args.tape, tape_base60, 60)
    tape_base50 = tapes_dir / 'potears2_50.lgtape'
    slice_tape(args.tape, tape_base50, 50)
    tape_base40 = tapes_dir / 'potears2_40.lgtape'
    slice_tape(args.tape, tape_base40, 40)
    tape_quit = tapes_dir / 'potears2_quit30.lgtape'
    slice_tape(args.tape, tape_quit, 60, quit_frame=30)
    tape_quit59 = tapes_dir / 'potears2_quit59.lgtape'
    slice_tape(args.tape, tape_quit59, 60, quit_frame=55)

    lag_overrides = dict(skin_workers=1, sim_overlap=True, sim_overlap_maps=['potears2'])
    write_env = dict(LOOKING_GLASS_REPLAY_WRITES='1', LOOKING_GLASS_SAVE_FIXED_AT=FIXED_SAVED_AT)
    results = []

    def run_pair(name, tape, extra_overrides=None, extra_env=None, seed_files=None,
                 expected_files=(), fixed_saved_at_files=None, expect_jpeg=True,
                 min_retained=10, reload_spec=None, keep_runs=False):
        off_ov = dict(extra_overrides or {})
        lag_ov = dict(off_ov, **lag_overrides)
        env = dict(extra_env or {})
        off = run_viewer(
            args.binary, args.data, args.config, args.sim, out / f'{name}_off',
            tape_src=tape, overrides=off_ov, extra_env=env, seed_files=seed_files,
        )
        lag = run_viewer(
            args.binary, args.data, args.config, args.sim, out / f'{name}_lag',
            tape_src=tape, overrides=lag_ov, extra_env=env, seed_files=seed_files,
        )
        assert lag['retained_frames'] >= min_retained, (
            f'{name}: expected >= {min_retained} retained frames, got {lag["retained_frames"]}'
        )
        compare_channels(off, lag, name)
        file_summary = compare_files(
            off, lag, name, expected_files, expect_jpeg=expect_jpeg,
            fixed_saved_at_files=fixed_saved_at_files,
        )
        reload_summary = None
        if reload_spec is not None:
            rel_file, slot_name, rmap = reload_spec
            reload_summary = verify_reload(
                args.binary, args.data, args.config, args.sim, out, name,
                slot_name, off['files'][rel_file]['raw'], lag['files'][rel_file]['raw'],
                lag_map=rmap,
            )
        row = dict(
            case=name,
            frames=off['frames'],
            lag_retained_frames=lag['retained_frames'],
            bytes_equal=True,
            channels_exact=True,
            files=file_summary,
            reload=reload_summary,
            retry_restored=dict(off=off['retry_restored'], lag=lag['retry_restored']),
            elapsed_s=round(off['elapsed_s'] + lag['elapsed_s'], 2),
        )
        results.append(row)
        print(
            f'PASS {name:28s} frames={off["frames"]:2d} retained={lag["retained_frames"]:2d} '
            f'files={len(file_summary)} reload={bool(reload_summary)} {row["elapsed_s"]:.1f}s',
            flush=True,
        )
        if not keep_runs:
            shutil.rmtree(out / f'{name}_off')
            shutil.rmtree(out / f'{name}_lag')
        return off, lag, row

    # 1. Discard control: replay without LOOKING_GLASS_REPLAY_WRITES leaves save dir empty.
    off_d, lag_d, _ = run_pair(
        'discard_control', tape_f5, extra_env={}, expected_files=(), expect_jpeg=False,
    )
    assert not off_d['files'] and not lag_d['files'], 'discard control wrote files'

    # 2. F5 quick save (frame 30 + frame 60 overwrite -> quick.json + quick.previous.json + auto.json).
    off_f5, lag_f5, _ = run_pair(
        'f5_quick_save', tape_f5, extra_env=write_env,
        expected_files=('quick.json', 'quick.previous.json', 'auto.json'),
        fixed_saved_at_files=('quick.json', 'quick.previous.json', 'auto.json'),
        reload_spec=('quick.json', 'quick', 'potears2'),
    )
    assert off_f5['files']['quick.json']['jpeg_sha256'] != off_f5['files']['quick.previous.json']['jpeg_sha256'], (
        'F5 overwrite should capture a fresh JPEG distinct from .previous'
    )
    # 3. Menu save (frame 30 + frame 60 -> slot1.json + slot1.previous.json + auto.json).
    off_ms, _, _ = run_pair(
        'menu_save', tape_base80,
        extra_env=dict(write_env, LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS='30:menu-save,60:menu-save'),
        expected_files=('slot1.json', 'slot1.previous.json', 'auto.json'),
        fixed_saved_at_files=('slot1.json', 'slot1.previous.json', 'auto.json'),
        reload_spec=('slot1.json', 'slot1', 'potears2'),
    )
    assert off_ms['files']['slot1.json']['jpeg_sha256'] != off_ms['files']['slot1.previous.json']['jpeg_sha256']

    # 4. New-game preserve (frame 30 preserves potears2 to auto.json -> transitions to gvillage -> exit writes gvillage to auto.json, rotating preserved potears2 to auto.previous.json).
    off_ng, _, _ = run_pair(
        'new_game_preserve', tape_base40,
        extra_overrides=dict(story_film_bypass=True),
        extra_env=dict(write_env, LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS='30:new-game'),
        expected_files=('auto.json', 'auto.previous.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json'),
        reload_spec=('auto.previous.json', 'auto', 'potears2'),
    )
    assert off_ng['files']['auto.previous.json']['current'].split('$')[0] == 'potears2', (
        f'expected preserved potears2 in auto.previous.json, got {off_ng["files"]["auto.previous.json"]["current"]}'
    )
    assert off_ng['files']['auto.json']['current'].split('$')[0] == 'gvillage', (
        f'expected new-game gvillage in auto.json, got {off_ng["files"]["auto.json"]["current"]}'
    )

    # 5. Arrival autosave via natural exit (potears2 -> skool1 writes auto.json, tape end rotates it to auto.previous.json).
    off_arr, _, _ = run_pair(
        'arrival_autosave', tape_base50,
        extra_env=dict(
            write_env,
            LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS='30:natural-exit',
            LOOKING_GLASS_TRANSITION_DESTINATION='skool1',
        ),
        expected_files=('auto.json', 'auto.previous.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json'),
        reload_spec=('auto.previous.json', 'auto', None),
    )
    assert off_arr['files']['auto.previous.json']['current'].split('$')[0] == 'skool1', (
        f'expected arrival skool1 in auto.previous.json, got {off_arr["files"]["auto.previous.json"]["current"]}'
    )

    # 6. Quit autosave (throttled clean capture via tape quit on frame 30; prior checkpoint on frame 15 exercises .previous rotation).
    run_pair(
        'quit_autosave_throttled', tape_quit,
        extra_env=dict(write_env, LOOKING_GLASS_SIM_FORCE_CHECKPOINT='15'),
        expected_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json'),
        reload_spec=('auto.json', 'auto', 'potears2'),
    )

    # 7. Quit autosave (immediate lag quit on frame 30 while frame 29 is retained; prior checkpoint on frame 15 exercises .previous rotation).
    run_pair(
        'quit_autosave_immediate', tape_base60,
        extra_env=dict(write_env, LOOKING_GLASS_SIM_FORCE_CHECKPOINT='15', LOOKING_GLASS_SIM_LAG_QUIT='30'),
        expected_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json'),
        reload_spec=('auto.json', 'auto', 'potears2'),
    )

    # 8. Forced finale checkpoint with writer OFF (frames 20 & 30 exercise finale-checkpoint .previous rotation).
    off_cp_off, _, _ = run_pair(
        'checkpoint_writer_off', tape_base60,
        extra_overrides=dict(finale_checkpoint_background=False),
        extra_env=dict(write_env, LOOKING_GLASS_SIM_FORCE_CHECKPOINT='20,30'),
        expected_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json', 'finale-checkpoint/auto.previous.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json', 'finale-checkpoint/auto.previous.json'),
        reload_spec=('finale-checkpoint/auto.json', 'auto', 'potears2'),
    )

    # 9. Forced finale checkpoint with writer ON (frames 20 & 30 -> byte-identical to writer OFF).
    off_cp_on, _, _ = run_pair(
        'checkpoint_writer_on', tape_base60,
        extra_overrides=dict(finale_checkpoint_background=True),
        extra_env=dict(write_env, LOOKING_GLASS_SIM_FORCE_CHECKPOINT='20,30'),
        expected_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json', 'finale-checkpoint/auto.previous.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json', 'finale-checkpoint/auto.previous.json'),
        reload_spec=('finale-checkpoint/auto.json', 'auto', 'potears2'),
    )
    assert off_cp_on['files']['finale-checkpoint/auto.json']['sha256'] == off_cp_off['files']['finale-checkpoint/auto.json']['sha256'], (
        'writer-on finale checkpoint bytes must match writer-off'
    )
    assert off_cp_on['files']['finale-checkpoint/auto.previous.json']['sha256'] == off_cp_off['files']['finale-checkpoint/auto.previous.json']['sha256'], (
        'writer-on finale .previous bytes must match writer-off'
    )

    # 10. Delayed writer success (held across frames 31-32, completes frame 33, retry at frame 45).
    off_ds, lag_ds, row_ds = run_pair(
        'checkpoint_delayed_success', tape_quit59,
        extra_overrides=dict(finale_checkpoint_background=True),
        extra_env=dict(
            write_env,
            LOOKING_GLASS_SIM_FORCE_CHECKPOINT='30',
            LOOKING_GLASS_CHECKPOINT_WRITER_TEST='delay:3',
            LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS='45:retry',
        ),
        expected_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json'),
        fixed_saved_at_files=('auto.json', 'auto.previous.json', 'finale-checkpoint/auto.json'),
        reload_spec=('finale-checkpoint/auto.json', 'auto', 'potears2'),
    )
    fenced_ds = [c['frame'] for c in lag_ds['census'] if 'pending_checkpoint' in c['w0_failed']]
    assert fenced_ds == [31, 32], f'expected pending_checkpoint fence on [31, 32], got {fenced_ds}'
    assert off_ds['retry_restored'] == 1 and lag_ds['retry_restored'] == 1
    assert off_ds['files']['finale-checkpoint/auto.json']['sha256'] == off_cp_off['files']['finale-checkpoint/auto.json']['sha256'], (
        'delayed-success finale checkpoint bytes must match writer-off'
    )
    row_ds['pending_checkpoint_fenced_frames'] = fenced_ds

    # 11. Delayed writer failure (held across frames 31-32, fails frame 33, retry at frame 45 restores pre-checkpoint retry).
    off_df, lag_df, row_df = run_pair(
        'checkpoint_delayed_failure', tape_quit59,
        extra_overrides=dict(finale_checkpoint_background=True),
        extra_env=dict(
            write_env,
            LOOKING_GLASS_SIM_FORCE_CHECKPOINT='30',
            LOOKING_GLASS_CHECKPOINT_WRITER_TEST='fail:3',
            LOOKING_GLASS_SIM_LAG_SAVE_ACTIONS='45:retry',
        ),
        expected_files=('auto.json',),
        fixed_saved_at_files=('auto.json',),
        reload_spec=('auto.json', 'auto', 'potears2'),
    )
    fenced_df = [c['frame'] for c in lag_df['census'] if 'pending_checkpoint' in c['w0_failed']]
    assert fenced_df == [31, 32], f'expected pending_checkpoint fence on [31, 32], got {fenced_df}'
    assert off_df['retry_restored'] == 1 and lag_df['retry_restored'] == 1
    assert 'finale-checkpoint/auto.json' not in off_df['files'], (
        'failed delayed checkpoint must not create finale checkpoint file'
    )
    assert 'auto.previous.json' not in off_df['files'], (
        'failed delayed checkpoint must not rotate auto.previous.json'
    )
    assert any('injected checkpoint writer failure' in n for n in off_df['notices']), (
        'expected injected failure notice in delayed failure run'
    )
    # Confirm post-retry simulation differs between delayed success (frame-30 retry) and delayed failure (frame-0 retry).
    ds_post = [s for s in off_ds['signatures'] if s[0] >= 46]
    df_post = [s for s in off_df['signatures'] if s[0] >= 46]
    assert ds_post and df_post and ds_post != df_post, (
        'delayed success and delayed failure must restore distinct retry states on death retry'
    )
    row_df['pending_checkpoint_fenced_frames'] = fenced_df
    row_df['retry_distinct_from_success'] = True

    shutil.rmtree(tapes_dir)
    receipt = dict(
        binary=str(args.binary),
        binary_sha256=sha256_bytes(Path(args.binary).read_bytes()),
        fixed_saved_at=int(FIXED_SAVED_AT),
        cases=results,
        writer_off_vs_on_finale_equal=True,
        writer_off_vs_delayed_finale_equal=True,
    )
    receipt_path = out / 'save-proofs.json'
    receipt_path.write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
    print(f'ALL {len(results)} SAVE PROOF CASES PASSED -> {receipt_path}', flush=True)


if __name__ == '__main__':
    main()
