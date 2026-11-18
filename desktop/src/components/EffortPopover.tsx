import { ChevronUp, Gauge } from "lucide-react";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

export interface EffortLevelDef {
  id: "low" | "medium" | "high" | "max" | "ultracode";
  name: string;
  wireValue: string;
  desc: string;
  warning?: string;
}

export const EFFORT_LEVELS: EffortLevelDef[] = [
  {
    id: "low",
    name: "Low",
    wireValue: "low",
    desc: "Quick answers with minimal deliberation.",
  },
  {
    id: "medium",
    name: "Medium",
    wireValue: "medium",
    desc: "Balanced speed and reasoning for everyday tasks.",
  },
  {
    id: "high",
    name: "High",
    wireValue: "high",
    desc: "Deeper reasoning for complex problems.",
  },
  {
    id: "max",
    name: "Max",
    wireValue: "max",
    desc: "Maximum reasoning. Uses your limits faster.",
    warning:
      "Model thinks longer with Max effort and uses your limits faster than High.",
  },
  {
    id: "ultracode",
    name: "Ultracode",
    wireValue: "max",
    desc: "Highest effort, plus dynamic workflows for this session.",
  },
];

const N = EFFORT_LEVELS.length;
const ULTRA_INDEX = N - 1;
const MAX_INDEX = N - 2;

export function resolveEffortIndex(
  effort: string | null | undefined,
  ultracodeEnabled: boolean
): number {
  if (ultracodeEnabled) {
    return ULTRA_INDEX;
  }
  switch ((effort ?? "medium").toLowerCase()) {
    case "low":
      return 0;
    case "high":
      return 2;
    case "xhigh":
    case "max":
      return 3;
    default:
      return 1;
  }
}

const clamp = (n: number, min: number, max: number) =>
  Math.min(max, Math.max(min, n));
const smooth = (t: number) => t * t * (3 - 2 * t);

const GRID_VS = `#version 300 es
in vec2 aPos;
uniform vec2 uRes;
out vec2 vUv;

void main() {
  vUv = (aPos * 0.5 + 0.5) * uRes;
  gl_Position = vec4(aPos, 0.0, 1.0);
}
`;

const GRID_FS = `#version 300 es
precision highp float;

uniform vec2  uRes;
uniform float uTime;
uniform float uFade;
uniform float uPos;
uniform float uSeed;

uniform vec3  uHot;
uniform vec3  uCool;
uniform vec4  uCharge0;
uniform vec4  uCharge1;

uniform float uBurstTime[8];
uniform vec2  uBurstCenter[8];
uniform float uBurstGain[8];

in vec2 vUv;
out vec4 outColor;

float hash21(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

float cellSdf(vec2 p, vec2 center, float halfSize, float radius) {
  vec2 q = abs(p - center) - vec2(halfSize - radius);
  return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
  const float PITCH = 4.0;
  const float SIZE = 3.0;
  const float RADIUS = 0.9;
  const int BURSTS = 8;

  float pitch = uRes.y / max(round(uRes.y / PITCH), 1.0);
  float cellScale = pitch / PITCH;
  vec2 cell = floor(vUv / pitch);
  vec2 cellCenter = (cell + 0.5) * pitch;

  float halfSize = SIZE * 0.5 * cellScale;
  float radius = RADIUS * cellScale;
  float dist = cellSdf(vUv, cellCenter, halfSize, radius);
  float aa = min(fwidth(vUv.x), 1.5);
  float cellMask = 1.0 - smoothstep(-aa, aa, dist);

  if (cellMask <= 0.0) {
    outColor = vec4(0.0);
    return;
  }

  float jitter = (hash21(cell + 41.7 + uSeed) - 0.5) * 0.2;
  float vary = 0.35 + 0.65 * hash21(cell + 13.7 + uSeed);

  float energy = 0.0;
  float charged = 0.0;

  for (int i = 0; i < BURSTS; i++) {
    float age = uTime - uBurstTime[i];
    if (age < 0.0 || age > 5.0) continue;

    float k = clamp(uBurstGain[i], 0.0, 1.0);
    vec2 originCell = floor(uBurstCenter[i] / pitch);
    float manhattan = abs(cell.x - originCell.x) + abs(cell.y - originCell.y);

    float range = mix(10.0, 55.0, k);
    float falloff = exp(-manhattan / (range * 0.85));
    float reach = exp(-max(manhattan - range, 0.0) * 0.12);
    float speed = mix(20.0, 36.0, k);
    float t = age - manhattan / speed - jitter;

    charged = max(charged, smoothstep(0.0, 0.15, t) * reach);
    if (t < 0.0) continue;

    float rank = hash21(cell + 7.3 + uSeed + uBurstTime[i]);
    if (rank > mix(0.30, 0.85, k)) continue;

    float head = 1.25 * exp(-t * 5.0);
    float body = 0.65 * exp(-t * mix(3.3, 0.77, k));
    energy += (head + body) * mix(0.55, 1.0, k) * falloff * reach * vary;
  }

  float lumA = hash21(cell + 8.8 + uSeed);
  float lumB = hash21(cell + 88.8 + uSeed);
  float drift = 0.5 + 0.5 * sin(uTime * 0.35 + lumA * 6.2832);
  float lum = pow(mix(lumA, lumB, drift), 1.9);
  float stream = 0.9 + 0.16 * sin(uTime * 1.3 + cell.x * 0.45 + lumB * 2.0);
  float e = clamp(energy, 0.0, 1.0) * uFade * mix(0.16, 1.0, lum) * stream;

  float g = clamp(vUv.x / max(uPos * uRes.x, 1.0), 0.0, 1.0);
  vec3 chargeColor = mix(uCharge0.rgb, uCharge1.rgb, g);
  float chargeA = 0.85 * pow(g, 1.1) * charged * uFade * cellMask;

  float lv = step(0.04, e) + step(0.20, e) + step(0.40, e)
           + step(0.60, e) + step(0.80, e);
  float alpha = lv <= 0.0 ? 0.0 : mix(0.08, 0.85, (lv - 1.0) / 4.0);

  if (lv >= 5.0) {
    float period = 2.4 + 1.4 * hash21(cell + 3.1 + uSeed);
    float phase = hash21(cell + 5.5 + uSeed) * 6.2832;
    alpha *= 1.0 - 0.14 * (1.0 + sin(uTime * 6.2832 / period + phase));
  }

  vec3 ink = mix(uCool, uHot, pow(lv / 5.0, 1.4));
  float eA = alpha * cellMask;
  vec3 col = ink * eA + chargeColor * chargeA * (1.0 - eA);
  float a = eA + chargeA * (1.0 - eA);

  if (a <= 0.0) {
    outColor = vec4(0.0);
    return;
  }

  outColor = vec4(col, a);
}
`;

class GridField {
  canvas: HTMLCanvasElement;
  gl: WebGL2RenderingContext;
  program: WebGLProgram;
  buffer: WebGLBuffer;
  aPos: number;
  u: Record<string, WebGLUniformLocation | null> = {};
  cssW = 1;
  cssH = 1;
  pxW = 1;
  pxH = 1;
  pos = 1;
  lit = false;
  fade = 0;
  seed = 0;
  slot = 0;
  pressElapsed = 0;
  nextBurst = 0;
  drawn = false;
  dpr = 1;
  burstTimes = new Float32Array(8).fill(-1000);
  burstCenters = new Float32Array(16);
  burstGains = new Float32Array(8).fill(0);

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const gl = canvas.getContext("webgl2", {
      alpha: true,
      antialias: false,
      depth: false,
      stencil: false,
      premultipliedAlpha: true,
      powerPreference: "low-power",
    });
    if (!gl) {
      throw new Error("WebGL2 is not available");
    }
    this.gl = gl;

    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.BLEND);
    gl.clearColor(0, 0, 0, 0);

    const vs = this.compile(gl.VERTEX_SHADER, GRID_VS);
    const fs = this.compile(gl.FRAGMENT_SHADER, GRID_FS);
    const program = gl.createProgram();
    if (!program) {
      throw new Error("Failed to create WebGL2 program");
    }
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      const log = gl.getProgramInfoLog(program);
      gl.deleteProgram(program);
      throw new Error(`Grid program link failed: ${log ?? "unknown"}`);
    }
    this.program = program;
    this.aPos = gl.getAttribLocation(program, "aPos");

    for (const name of [
      "uRes",
      "uTime",
      "uFade",
      "uPos",
      "uSeed",
      "uHot",
      "uCool",
      "uCharge0",
      "uCharge1",
      "uBurstTime[0]",
      "uBurstCenter[0]",
      "uBurstGain[0]",
    ]) {
      this.u[name] = gl.getUniformLocation(program, name);
    }

    const buffer = gl.createBuffer();
    if (!buffer) {
      throw new Error("Failed to create WebGL2 buffer");
    }
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]),
      gl.STATIC_DRAW
    );
    this.buffer = buffer;

    gl.useProgram(program);
    gl.uniform3f(this.u.uHot, 0.627, 0.588, 0.922);
    gl.uniform3f(this.u.uCool, 1.0, 1.0, 1.0);
    gl.uniform4f(this.u.uCharge0, 0.62, 0.5, 0.92, 1.0);
    gl.uniform4f(this.u.uCharge1, 0.35, 0.25, 0.6, 1.0);
    this.resize(1, 1, 1);
  }

  private compile(type: number, source: string): WebGLShader {
    const { gl } = this;
    const shader = gl.createShader(type);
    if (!shader) {
      throw new Error("Failed to allocate WebGL2 shader");
    }
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const log = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader);
      throw new Error(`Grid shader compile failed: ${log ?? "unknown"}`);
    }
    return shader;
  }

  resize(cssWidth: number, cssHeight: number, dpr: number) {
    const { gl } = this;
    const w = Math.max(1, cssWidth);
    const h = Math.max(1, cssHeight);
    const nextDpr = clamp(dpr || 1, 1, 2);
    const pxW = Math.max(1, Math.round(w * nextDpr));
    const pxH = Math.max(1, Math.round(h * nextDpr));

    if (this.cssW > 1) {
      const sx = w / this.cssW;
      const sy = h / this.cssH;
      for (let i = 0; i < 8; i++) {
        this.burstCenters[i * 2] *= sx;
        this.burstCenters[i * 2 + 1] *= sy;
      }
    }

    this.cssW = w;
    this.cssH = h;
    this.dpr = nextDpr;
    if (this.pxW === pxW && this.pxH === pxH) {
      this.drawn = false;
      return;
    }

    this.pxW = pxW;
    this.pxH = pxH;
    this.canvas.width = pxW;
    this.canvas.height = pxH;
    gl.viewport(0, 0, pxW, pxH);
    gl.clear(gl.COLOR_BUFFER_BIT);
    this.drawn = false;
  }

  setPos(pos: number) {
    this.pos = clamp(pos, 0.02, 1);
  }

  setLit(lit: boolean) {
    if (lit === this.lit) return;
    this.lit = lit;
    if (!lit) return;

    const now = performance.now() / 1000;
    this.seed = Math.random() * 512;
    this.burstTimes.fill(-1000);
    this.burstGains.fill(0);
    this.pressElapsed = 0;
    this.fade = 0;
    this.nextBurst = now;
    this.spawnBurst(now);
    this.nextBurst = now + 0.3 + Math.random() * 0.45;
  }

  private spawnBurst(now: number) {
    const base = Math.min(
      1,
      (0.15 + 0.7 * this.pos) *
        (0.45 + 0.55 * Math.min(this.pressElapsed / 1.6, 1))
    );
    const gain = Math.min(1, base * (0.85 + Math.random() * 0.3));
    const i = this.slot;
    this.slot = (this.slot + 1) % 8;
    this.burstTimes[i] = now;
    this.burstCenters[i * 2] = this.pos * this.cssW;
    this.burstCenters[i * 2 + 1] = (0.35 + Math.random() * 0.3) * this.cssH;
    this.burstGains[i] = gain;
  }

  render(time: number, dt: number) {
    const targetLit = this.lit;
    if (targetLit) this.pressElapsed += dt;

    const rate = targetLit ? dt / 0.45 : dt / 0.85;
    this.fade = clamp(this.fade + (targetLit ? rate : -rate), 0, 1);

    if (targetLit) {
      while (time >= this.nextBurst) {
        this.spawnBurst(this.nextBurst);
        this.nextBurst += 0.3 + Math.random() * 0.45;
      }
    }

    if (this.fade <= 0.002 && !targetLit) {
      if (this.drawn) {
        this.gl.clear(this.gl.COLOR_BUFFER_BIT);
        this.drawn = false;
      }
      return;
    }

    const { gl } = this;
    if (this.pxW < 1) return;
    gl.useProgram(this.program);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.enableVertexAttribArray(this.aPos);
    gl.vertexAttribPointer(this.aPos, 2, gl.FLOAT, false, 0, 0);

    gl.uniform2f(this.u.uRes, this.cssW, this.cssH);
    gl.uniform1f(this.u.uTime, time);
    gl.uniform1f(this.u.uFade, smooth(this.fade));
    gl.uniform1f(this.u.uPos, this.pos);
    gl.uniform1f(this.u.uSeed, this.seed);
    gl.uniform1fv(this.u["uBurstTime[0]"], this.burstTimes);
    gl.uniform2fv(this.u["uBurstCenter[0]"], this.burstCenters);
    gl.uniform1fv(this.u["uBurstGain[0]"], this.burstGains);

    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.drawArrays(gl.TRIANGLES, 0, 6);
    this.drawn = true;
  }

  dispose() {
    try {
      this.gl.deleteBuffer(this.buffer);
      this.gl.deleteProgram(this.program);
    } catch {
      // Ignore context loss on cleanup.
    }
  }
}

export interface EffortPillProps {
  effort: string | null | undefined;
  ultracodeEnabled: boolean;
  open: boolean;
  disabled?: boolean;
  onClick: () => void;
}

export function EffortPill({
  effort,
  ultracodeEnabled,
  open,
  disabled,
  onClick,
}: EffortPillProps) {
  const index = resolveEffortIndex(effort, ultracodeEnabled);
  const level = EFFORT_LEVELS[index];
  const isUltra = index === ULTRA_INDEX;

  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      aria-expanded={open}
      aria-label={`Effort: ${level.name}. Open effort settings.`}
      data-testid="effort-pill"
      className={`inline-flex h-[26px] items-center gap-1.5 rounded-[7px] border px-[9px] text-[11px] transition-colors disabled:opacity-40 ${
        isUltra
          ? "border-[#a096eb]/30 bg-[#a096eb]/10 text-[#d5ceff] shadow-[0_0_20px_-9px_rgba(160,150,235,0.65)]"
          : "border-[#2e2e2e] bg-[#1c1c1c] text-[#d8d3ca] hover:border-[#3e3e3e] hover:text-[#e8e8e8]"
      }`}
    >
      <Gauge
        className={`h-3 w-3 shrink-0 ${
          isUltra ? "text-[#b9aeff]" : "text-[#8a8a8a]"
        }`}
      />
      <span>Effort: {level.name}</span>
      <ChevronUp
        className={`h-3 w-3 shrink-0 opacity-60 transition-transform ${
          open ? "rotate-180" : ""
        }`}
      />
    </button>
  );
}

export interface EffortPopoverProps {
  open: boolean;
  effort: string | null | undefined;
  ultracodeEnabled: boolean;
  disabled?: boolean;
  onSelectEffort: (effort: string, ultracode: boolean) => Promise<boolean>;
  onClose?: () => void;
}

export function EffortPopover({
  open,
  effort,
  ultracodeEnabled,
  disabled,
  onSelectEffort,
  onClose,
}: EffortPopoverProps) {
  const externalIndex = resolveEffortIndex(effort, ultracodeEnabled);
  const [selectedIndex, setSelectedIndex] = useState(externalIndex);
  const [dragging, setDragging] = useState(false);
  const [noRing, setNoRing] = useState(false);
  const [noWebgl, setNoWebgl] = useState(false);
  const [thumbPx, setThumbPx] = useState(0);
  const [fillEndPx, setFillEndPx] = useState(10);
  const [stopCenters, setStopCenters] = useState<number[]>([
    10, 62, 114, 166, 218,
  ]);

  const popoverRef = useRef<HTMLElement | null>(null);
  const sliderRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const thumbRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const gridRef = useRef<GridField | null>(null);

  const posRef = useRef(externalIndex / (N - 1));
  const selectedRef = useRef(externalIndex);
  const springingRef = useRef(false);
  const springTargetRef = useRef(externalIndex / (N - 1));
  const springVelRef = useRef(0);
  const draggingRef = useRef(false);
  const pointerIdRef = useRef<number | null>(null);
  const grabOffsetRef = useRef(0);
  const geometryRef = useRef({
    width: 244,
    height: 24,
    thumbWidth: 20,
    travel: 224,
  });

  const reduceMotion =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  const updateGeometryState = useCallback(() => {
    const geom = geometryRef.current;
    const tx = posRef.current * geom.travel;
    const fe = geom.thumbWidth / 2 + posRef.current * geom.travel;
    setThumbPx(tx);
    setFillEndPx(fe);
    if (gridRef.current) {
      gridRef.current.setPos(fe / Math.max(1, geom.width));
    }
  }, []);

  const measure = useCallback(() => {
    const trackEl = trackRef.current;
    const thumbEl = thumbRef.current;
    if (!trackEl) return;
    const trackWidth = trackEl.clientWidth || 244;
    const trackHeight = trackEl.clientHeight || 24;
    const thumbWidth = thumbEl?.offsetWidth || 20;
    const travel = Math.max(1, trackWidth - thumbWidth);

    geometryRef.current = {
      width: trackWidth,
      height: trackHeight,
      thumbWidth,
      travel,
    };

    const nextStops = EFFORT_LEVELS.map(
      (_, idx) => thumbWidth / 2 + (idx * travel) / (N - 1)
    );
    setStopCenters(nextStops);

    if (gridRef.current) {
      gridRef.current.resize(
        trackWidth,
        trackHeight,
        Math.min(window.devicePixelRatio || 1, 2)
      );
    }
    updateGeometryState();
  }, [updateGeometryState]);

  const commitIndex = useCallback(
    (nextIdx: number) => {
      const clamped = clamp(nextIdx, 0, N - 1);
      selectedRef.current = clamped;
      setSelectedIndex(clamped);
      const lvl = EFFORT_LEVELS[clamped];
      if (gridRef.current) {
        gridRef.current.setLit(clamped === ULTRA_INDEX);
      }
      void onSelectEffort(lvl.wireValue, clamped === ULTRA_INDEX);
    },
    [onSelectEffort]
  );

  const startSpring = useCallback(
    (nextIdx: number, commit = true) => {
      const clamped = clamp(nextIdx, 0, N - 1);
      selectedRef.current = clamped;
      setSelectedIndex(clamped);
      springTargetRef.current = clamped / (N - 1);
      springVelRef.current = 0;

      if (gridRef.current) {
        gridRef.current.setLit(clamped === ULTRA_INDEX);
      }

      if (reduceMotion) {
        posRef.current = springTargetRef.current;
        springingRef.current = false;
        updateGeometryState();
      } else {
        springingRef.current = true;
      }

      if (commit) {
        const lvl = EFFORT_LEVELS[clamped];
        void onSelectEffort(lvl.wireValue, clamped === ULTRA_INDEX);
      }
    },
    [onSelectEffort, reduceMotion, updateGeometryState]
  );

  // Sync when store changes externally
  useEffect(() => {
    if (draggingRef.current) return;
    selectedRef.current = externalIndex;
    setSelectedIndex(externalIndex);
    const target = externalIndex / (N - 1);
    springTargetRef.current = target;
    posRef.current = target;
    updateGeometryState();
    if (gridRef.current) {
      gridRef.current.setLit(externalIndex === ULTRA_INDEX);
    }
  }, [externalIndex, updateGeometryState]);

  const isJsdom =
    typeof navigator !== "undefined" && /jsdom/i.test(navigator.userAgent);

  // Boot WebGL2 grid and animation loop when open
  useEffect(() => {
    if (!open) {
      gridRef.current?.dispose();
      gridRef.current = null;
      return;
    }

    measure();

    if (reduceMotion || isJsdom) {
      setNoWebgl(true);
    } else if (canvasRef.current) {
      try {
        const gf = new GridField(canvasRef.current);
        const geom = geometryRef.current;
        gf.resize(
          geom.width,
          geom.height,
          Math.min(window.devicePixelRatio || 1, 2)
        );
        gf.setPos((geom.thumbWidth / 2 + posRef.current * geom.travel) / Math.max(1, geom.width));
        gf.setLit(selectedRef.current === ULTRA_INDEX);
        gridRef.current = gf;
        setNoWebgl(false);
      } catch {
        gridRef.current = null;
        setNoWebgl(true);
      }
    } else {
      setNoWebgl(true);
    }

    let rafId = 0;
    let lastTime = performance.now();

    const stepFrame = (now: number) => {
      const dt = clamp((now - lastTime) / 1000, 0, 0.05);
      lastTime = now;

      if (springingRef.current) {
        const stiffness = 520;
        const damping = 38;
        const steps = Math.max(1, Math.ceil(dt / 0.006));
        const h = dt / steps;

        for (let i = 0; i < steps; i++) {
          const accel =
            stiffness * (springTargetRef.current - posRef.current) -
            damping * springVelRef.current;
          springVelRef.current += accel * h;
          posRef.current += springVelRef.current * h;

          if (posRef.current <= 0) {
            posRef.current = 0;
            if (springVelRef.current < 0) springVelRef.current = 0;
          } else if (posRef.current >= 1) {
            posRef.current = 1;
            if (springVelRef.current > 0) springVelRef.current = 0;
          }

          if (
            Math.abs(springTargetRef.current - posRef.current) < 0.0004 &&
            Math.abs(springVelRef.current) < 0.02
          ) {
            posRef.current = springTargetRef.current;
            springVelRef.current = 0;
            springingRef.current = false;
            break;
          }
        }
        updateGeometryState();
      }

      if (gridRef.current) {
        gridRef.current.render(now / 1000, dt);
      }

      rafId = requestAnimationFrame(stepFrame);
    };

    rafId = requestAnimationFrame(stepFrame);

    let observer: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined" && trackRef.current) {
      observer = new ResizeObserver(() => measure());
      observer.observe(trackRef.current);
    }
    window.addEventListener("resize", measure);

    return () => {
      cancelAnimationFrame(rafId);
      observer?.disconnect();
      window.removeEventListener("resize", measure);
      gridRef.current?.dispose();
      gridRef.current = null;
    };
  }, [open, measure, reduceMotion, isJsdom, updateGeometryState]);

  if (!open) {
    return null;
  }

  const posFromClientX = (clientX: number) => {
    const trackEl = trackRef.current;
    const geom = geometryRef.current;
    if (!trackEl) return posRef.current;
    const rect = trackEl.getBoundingClientRect();
    if (rect.width <= 0) return posRef.current;
    const x = clientX - rect.left - grabOffsetRef.current;
    return clamp((x - geom.thumbWidth / 2) / geom.travel, 0, 1);
  };

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (disabled) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    e.preventDefault();

    springingRef.current = false;
    springVelRef.current = 0;
    draggingRef.current = true;
    setDragging(true);
    pointerIdRef.current = e.pointerId;
    setNoRing(true);

    try {
      sliderRef.current?.focus({ preventScroll: true });
      sliderRef.current?.setPointerCapture(e.pointerId);
    } catch {
      // Ignore in test environments without full pointer capture.
    }

    const rect = trackRef.current?.getBoundingClientRect();
    const geom = geometryRef.current;
    if (rect && rect.width > 0) {
      const center = rect.left + geom.thumbWidth / 2 + posRef.current * geom.travel;
      const offset = e.clientX - center;
      grabOffsetRef.current =
        Math.abs(offset) <= (geom.thumbWidth * 0.7) / 2 ? offset : 0;
    }

    const nextPos = posFromClientX(e.clientX);
    posRef.current = nextPos;
    const nextIdx = clamp(Math.round(nextPos * (N - 1)), 0, N - 1);
    if (nextIdx !== selectedRef.current) {
      selectedRef.current = nextIdx;
      setSelectedIndex(nextIdx);
      gridRef.current?.setLit(nextIdx === ULTRA_INDEX);
    }
    updateGeometryState();
  };

  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current || e.pointerId !== pointerIdRef.current) return;
    e.preventDefault();
    const nextPos = posFromClientX(e.clientX);
    posRef.current = nextPos;
    const nextIdx = clamp(Math.round(nextPos * (N - 1)), 0, N - 1);
    if (nextIdx !== selectedRef.current) {
      selectedRef.current = nextIdx;
      setSelectedIndex(nextIdx);
      gridRef.current?.setLit(nextIdx === ULTRA_INDEX);
    }
    updateGeometryState();
  };

  const finishDrag = (e?: ReactPointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    if (e && e.pointerId !== pointerIdRef.current) return;
    draggingRef.current = false;
    setDragging(false);

    if (pointerIdRef.current !== null) {
      try {
        if (sliderRef.current?.hasPointerCapture?.(pointerIdRef.current)) {
          sliderRef.current.releasePointerCapture(pointerIdRef.current);
        }
      } catch {
        // Ignore
      }
    }
    pointerIdRef.current = null;
    grabOffsetRef.current = 0;
    startSpring(selectedRef.current, true);
  };

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (disabled) return;
    setNoRing(false);
    let next = selectedRef.current;

    switch (e.key) {
      case "ArrowRight":
      case "ArrowUp":
      case "PageUp":
        next = selectedRef.current + 1;
        break;
      case "ArrowLeft":
      case "ArrowDown":
      case "PageDown":
        next = selectedRef.current - 1;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = ULTRA_INDEX;
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        startSpring(selectedRef.current, true);
        return;
      case "Escape":
        e.preventDefault();
        onClose?.();
        return;
      default:
        return;
    }

    e.preventDefault();
    startSpring(clamp(next, 0, ULTRA_INDEX), true);
  };

  const activeLevel = EFFORT_LEVELS[selectedIndex];
  const isUltra = selectedIndex === ULTRA_INDEX;

  return (
    <section
      ref={popoverRef}
      aria-label="Effort settings"
      data-testid="effort-popover"
      style={
        {
          "--thumb-x": `${thumbPx.toFixed(2)}px`,
          "--fill-end": `${fillEndPx.toFixed(2)}px`,
        } as React.CSSProperties
      }
      className={`ccs-popover ${isUltra ? "is-ultra" : ""} ${
        noWebgl ? "no-webgl" : ""
      }`}
    >
      <div className="mb-3 flex min-h-[18px] items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <span className="text-[12px] font-medium tracking-[0.005em] text-[#8b877e]">
            Effort
          </span>
          <span className="ccs-level" data-testid="effort-level-label">
            {activeLevel.name}
          </span>
        </div>
        <button
          type="button"
          className="ccs-help"
          aria-label="About effort"
          data-tip="Faster on the left, smarter on the right. Choose Ultracode to add dynamic workflows."
        >
          ?
        </button>
      </div>

      <div
        className="mx-[2px] mb-[7px] flex items-center justify-between gap-2 text-[11px] text-[#8b877e]"
        aria-hidden="true"
      >
        <span>Faster</span>
        <span>Smarter</span>
      </div>

      <div
        ref={sliderRef}
        role="slider"
        tabIndex={0}
        aria-label="Effort slider"
        aria-valuemin={0}
        aria-valuemax={ULTRA_INDEX}
        aria-valuenow={selectedIndex}
        aria-valuetext={`${activeLevel.name}. ${activeLevel.warning ?? activeLevel.desc}`}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={finishDrag}
        onPointerCancel={finishDrag}
        onKeyDown={handleKeyDown}
        onBlur={() => setNoRing(false)}
        className={`ccs-slider ${dragging ? "is-dragging" : ""} ${
          noRing ? "no-ring" : ""
        }`}
      >
        <div ref={trackRef} className="ccs-track">
          <div className="ccs-fill" aria-hidden="true">
            <div className="ccs-fill-inner">
              <canvas ref={canvasRef} className="ccs-grid" aria-hidden="true" />
            </div>
          </div>
          <div className="ccs-stops">
            {EFFORT_LEVELS.map((lvl, idx) => (
              <button
                key={lvl.id}
                type="button"
                title={lvl.name}
                aria-label={`Set effort to ${lvl.name}`}
                style={{ left: `${stopCenters[idx] ?? 10}px` }}
                onPointerDown={(ev) => {
                  ev.stopPropagation();
                  if (!disabled) {
                    startSpring(idx, true);
                  }
                }}
                onClick={(ev) => {
                  ev.stopPropagation();
                  if (!disabled) {
                    commitIndex(idx);
                  }
                }}
                className="ccs-stop"
              >
                <span className="ccs-stop-dot" />
              </button>
            ))}
          </div>
          <div ref={thumbRef} className="ccs-thumb" aria-hidden="true" />
        </div>
      </div>

      <p
        className={`mx-[2px] mt-[10px] min-h-[34px] text-[11.5px] leading-[1.45] ${
          activeLevel.warning ? "text-[#fac454]" : "text-[#8b877e]"
        }`}
      >
        {activeLevel.warning ? (
          <span className="mr-1.5 inline-grid h-[11px] w-[11px] place-items-center rounded-full border border-current text-[8px] font-semibold leading-none">
            !
          </span>
        ) : null}
        {activeLevel.warning ?? activeLevel.desc}
      </p>

      <div className="mt-3 flex items-center justify-between gap-3 border-t border-white/[0.07] pt-3">
        <div className="min-w-0">
          <div
            className={`text-[12px] font-medium transition-colors ${
              isUltra ? "text-[#b9aeff]" : "text-[#c9c5bc]"
            }`}
          >
            Ultracode
          </div>
          <p className="mt-0.5 text-[10.5px] leading-[1.4] text-[#8b877e]">
            Standing dynamic-workflow orchestration for this session.
          </p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={isUltra}
          aria-label="Ultracode toggle"
          disabled={disabled}
          onClick={() => startSpring(isUltra ? MAX_INDEX : ULTRA_INDEX, true)}
          className={`devin-switch ${isUltra ? "is-ultra" : ""}`}
        >
          <span className="devin-switch__knob" />
        </button>
      </div>
    </section>
  );
}
