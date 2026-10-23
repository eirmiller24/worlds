> **Status:** reference material, not a requirement. Worlds matches LDtk's rule semantics in general (patterns, flips, modulo, checker, break, stamps, pivots, out-of-bounds, optional groups) but uses its own deterministic hash for chance, perlin seeding, tile alternatives and random offsets, and does not replicate the pruning quirk in §5. The parity test checks that the same rules fire on the same cells and that each placed tile is one of the rule's alternatives.

# LDtk 1.5.3 auto-layer parity spec

This is the contract for reproducing LDtk 1.5.3's baked `autoLayerTiles` exactly. Everything here comes from the compiled Haxe-to-JS bundle (`renderer_fixed.js`, 315,892 lines). Each section gives:

- the compiled JS lines, quoted verbatim with their line numbers;
- pseudocode in Rust-flavoured notation, with integer widths and overflow behaviour spelled out.

`Const.AUTO_LAYER_ANYTHING = 1000001` and `Const.MAX_AUTO_PATTERN_SIZE = 9`.

## 0. How JS numbers behave in this code (read this first)

Haxe `Int` compiles to a plain JS `number` (an f64). The compiler adds no `| 0` after arithmetic and never uses `Math.imul`. Every `+`, `-`, `*` and `/` you see below is therefore a **float64 operation**. The only int32 conversions are the explicit bit operators:

| JS op | Semantics | Rust equivalent |
|---|---|---|
| `a >> n` | `ToInt32(a) >> n`, signed (arithmetic) shift | `(a as i32) >> n` once `a` is reduced to i32 |
| `a >>> n` | `ToUint32(a) >>> n`, unsigned shift | `(a as u32) >> n` |
| `a ^ b`, `a & b`, `a \| 0` | `ToInt32` on both operands, result is i32 | `i32` ops |
| `x \| 0` on a float | truncate toward zero, then wrap mod 2^32 | `x.trunc() as i64 as i32` (for \|x\| < 2^31 this is just `x as i32`) |
| `a % b` | IEEE fmod: the sign follows the dividend; `b == 0` gives `NaN` | Rust `%` on integers has the same sign rule (truncating), but **panics** on 0 |
| `ToInt32(f)` where f is an integer-valued double | `f mod 2^32`, reinterpreted as i32 | `(f as i64) as i32` if \|f\| < 2^63 (truncating cast keeps the low 32 bits) |

**The ceil idiom.** LDtk computes all its `cWid`/`cHei` values with `dn.M.ceil`, which inlines to this:

```js
if(x > .0) { var t = x + .5 | 0; v = t < x ? t + 1 : t; }
else if(x < .0) { var t = x - .5 | 0; v = t < x ? t + 1 : t; }
else { v = 0; }
```

For `0 < x < 2^31` this equals `x.ceil()`. It is used for:

- **Layer grid width.** `cWid(li) = ceil((level.pxWid - li.pxOffsetX) / ld.gridSize)`, and the same for `cHei` with `pxHei`/`pxOffsetY`. Example at `renderer_fixed.js:38877`:
  `var x = ((...).pxWid - source.pxOffsetX) / (...).gridSize;`
- **Tileset grid width.** `tdCWid(td) = ceil((td.pxWid - td.padding*2) / (td.tileGridSize + td.spacing))`, and `tdCHei` the same with `pxHei`. **If `td.relPath == null && td.embedAtlas == null`, the code returns `0` instead.** Then `tid / 0 | 0` evaluates to `Infinity | 0 = 0`. Mirror that, don't panic.

**Integer division.** `a / b | 0` is float division followed by truncation toward zero. For non-negative operands that's the same as Rust `a / b`. For negative `a` it truncates, it doesn't floor: `-3 / 10 | 0 == 0`.

---

## 1. Chance test

`AutoLayerRuleDef.matches`, `renderer_fixed.js:38816-38833`:

```js
if(this.tileRectsIds.length == 0) {
	return false;
}
var tmp;
if(!(this.chance <= 0)) {
	if(this.chance < 1) {
		var h = li.seed + this.uid + cx * 374761393 + cy * 668265263;
		h = (h ^ h >> 13) * 1274126177;
		tmp = (h ^ h >> 16) % 100 >= this.chance * 100;
	} else {
		tmp = false;
	}
} else {
	tmp = true;
}
if(tmp) {
	return false;
}
```

`li` is the **target** layer instance (the auto-layer being filled), not the IntGrid source. `cx, cy` are the unflipped anchor cell. Flips don't feed this hash, so if the chance test fails for a cell it fails for all four flip attempts.

### The mix function (shared by sections 1-3)

Three things to get right:

- `>>` binds tighter than `^`, so `h ^ h >> 13` means `h ^ (h >> 13)`.
- Both operands are `ToInt32(h)`.
- The multiply by `1274126177` is a **float64 multiply** (no `Math.imul`), and that's the trap. The product reaches ~2^61, so f64 rounds it to 53 significant bits, and the next `^`/`>>` takes the low 32 bits *of the rounded double*.

```rust
/// h0 is the exact integer sum. JS does the sum in f64, which is exact while |h0| < 2^53,
/// i.e. for any sane cx, cy (< ~6.7 million) and 0 <= seed < 1e7.
fn ldtk_mix(h0: i64) -> i32 {
    let h: i32 = h0 as i32;                  // ToInt32: keep the low 32 bits (wraps)
    let a: i32 = h ^ (h >> 13);              // arithmetic shift. Bit 31 = h31 ^ h31 = 0, so a >= 0
    let p: f64 = (a as f64) * 1274126177.0;  // MUST be f64. 0 <= p < 2^62, rounded to 53 bits
    let q: i32 = (p as i64) as i32;          // p is integer-valued and < 2^63, so the cast is exact.
                                             // Then keep the low 32 bits = ToInt32(p)
    q ^ (q >> 16)                            // bit 31 = q31 ^ q31 = 0, so the result is always in [0, 2^31)
}
```

**Do not use `a.wrapping_mul(1274126177)`.** It disagrees with LDtk on ~98% of inputs. In a 200x200 sweep, 39,085 of 40,000 `%100` rolls differed. Example: seed=1234567, uid=42, cx=3, cy=5 gives a JS roll of **36**; the wrapping-mul version gives 82.

**Sign.** Because of the xor-shift, `mix()` is never negative (checked on 20,000 random inputs plus a 300x300 grid: zero negatives). So `% 100` gives 0..99 and `% n` never produces a negative array index.

### Chance pseudocode

```rust
fn chance_rejects(li_seed: i32, rule_uid: i32, cx: i32, cy: i32, chance: f64) -> bool {
    if chance <= 0.0 { return true; }
    if chance >= 1.0 { return false; }   // the hash only runs when chance < 1
    let h0 = li_seed as i64 + rule_uid as i64 + (cx as i64) * 374761393 + (cy as i64) * 668265263;
    let roll = ldtk_mix(h0) % 100;       // 0..=99
    (roll as f64) >= chance * 100.0      // compare in f64; do NOT round chance to an int percent
}
```

**Float trap.** `chance * 100` isn't always an integer. In JS, `0.07*100 = 7.000000000000001`, so rolls 0..7 pass: **8%, not 7%**. The same thing happens for 0.14, 0.28, 0.55 and 0.56. For 0.29 (28.999999999999996), 0.57 and 0.58 the result is just below the integer, which happens to give the intuitive count. Replicate this with an f64 multiply.

## 2. Random tile alternative selection

`renderer_fixed.js:39260-39268`:

```js
,getRandomTileRectIdsForCoord: function(seed,cx,cy,flips) {
	if(this.tileRectsIds.length == 0) {
		return [];
	} else {
		var h = this.uid + seed + flips + cx * 374761393 + cy * 668265263;
		h = (h ^ h >> 13) * 1274126177;
		return this.tileRectsIds[(h ^ h >> 16) % this.tileRectsIds.length];
	}
}
```

It's called with `seed = li.seed` (target layer) and `flips` = the flip attempt index 0, 1, 2 or 3 (see section 5). Example, `renderer_fixed.js:59066`:
`var tileRectIds = r.getRandomTileRectIdsForCoord(_gthis.seed,x,y,0);`

```rust
fn pick_alternative(rule: &Rule, li_seed: i32, cx: i32, cy: i32, flips: i32) -> &[i32] {
    let n = rule.tile_rects_ids.len();          // matches() already returned false if n == 0
    let h0 = rule.uid as i64 + li_seed as i64 + flips as i64 + (cx as i64)*374761393 + (cy as i64)*668265263;
    &rule.tile_rects_ids[(ldtk_mix(h0) as usize) % n]   // mix >= 0, so no negative index
}
```

Things to know:

- `flips` is in the hash, so each flip variant at the same cell can pick a different alternative.
- Each entry of `tileRectsIds` is itself an array of tile ids:
  - **Single mode:** usually one tid per entry.
  - **Stamp mode:** one entry holding the whole stamp.
  - **Legacy `tileIds` JSON:** converted at `38441-38459`. Single becomes `[[t0],[t1],...]`; Stamp becomes `[[t0,t1,...]]`.
- Every tid in the chosen entry is placed. There's no per-tid randomness.

## 3. Random pixel offsets

`renderer_fixed.js:39269-39290`:

```js
,getXOffsetForCoord: function(seed,cx,cy,flips) {
	var tmp;
	if(this.tileRandomXMin == 0 && this.tileRandomXMax == 0) {
		tmp = 0;
	} else {
		var h = this.uid + seed + flips + cx * 374761393 + cy * 668265263;
		h = (h ^ h >> 13) * 1274126177;
		tmp = (h ^ h >> 16) % (this.tileRandomXMax - this.tileRandomXMin + 1) + this.tileRandomXMin;
	}
	return ((flips & 1) != 0 ? -1 : 1) * (this.tileXOffset + tmp);
}
,getYOffsetForCoord: function(seed,cx,cy,flips) {
	var tmp;
	if(this.tileRandomYMin == 0 && this.tileRandomYMax == 0) {
		tmp = 0;
	} else {
		var h = this.uid + seed + 1 + cx * 374761393 + cy * 668265263;
		h = (h ^ h >> 13) * 1274126177;
		tmp = (h ^ h >> 16) % (this.tileRandomYMax - this.tileRandomYMin + 1) + this.tileRandomYMin;
	}
	return ((flips & 2) != 0 ? -1 : 1) * (this.tileYOffset + tmp);
}
```

```rust
fn x_offset(rule: &Rule, li_seed: i32, cx: i32, cy: i32, flips: i32) -> i32 {
    let rnd = if rule.rand_x_min == 0 && rule.rand_x_max == 0 { 0 } else {
        let h0 = rule.uid as i64 + li_seed as i64 + flips as i64 + (cx as i64)*374761393 + (cy as i64)*668265263;
        let n = rule.rand_x_max - rule.rand_x_min + 1;  // n == 0 gives NaN in JS (see "Where parity can break")
        ldtk_mix(h0) % n + rule.rand_x_min               // mix >= 0; for n < 0 the Rust % sign rule matches JS
    };
    (if flips & 1 != 0 { -1 } else { 1 }) * (rule.tile_x_offset + rnd)
}
fn y_offset(rule: &Rule, li_seed: i32, cx: i32, cy: i32, flips: i32) -> i32 {
    let rnd = if rule.rand_y_min == 0 && rule.rand_y_max == 0 { 0 } else {
        // NOTE: literal `+ 1` here, NOT `flips`
        let h0 = rule.uid as i64 + li_seed as i64 + 1 + (cx as i64)*374761393 + (cy as i64)*668265263;
        ldtk_mix(h0) % (rule.rand_y_max - rule.rand_y_min + 1) + rule.rand_y_min
    };
    (if flips & 2 != 0 { -1 } else { 1 }) * (rule.tile_y_offset + rnd)
}
```

Quirks to replicate:

- **The Y hash ignores `flips`.** It always uses `seed + 1`, so the random Y draw is the same for all four flip variants at a cell.
- **Shared hashes.** On the X-flip attempt (`flips == 1`) the X and Y hashes are identical, so X and Y random offsets are correlated. The alternative index (section 2) uses the same `h` as the X offset for the same `flips`.
- **Flips negate the whole offset**, the fixed `tileXOffset` and the random part together.
- **Gate.** The random part is skipped only when *both* min and max are 0. For example, min = -3, max = 0 still draws.

## 4. Perlin gate

`renderer_fixed.js:38834-38850`:

```js
if(this.perlinActive) {
	if(this.perlinSeed != null && this._perlin == null) {
		this._perlin = new hxd_Perlin();
		this._perlin.normalize = true;
		this._perlin.adjustScale(50,1);
	}
	...
	tmp = this._perlin.perlin(li.seed + this.perlinSeed,cx * this.perlinScale,cy * this.perlinScale,this.perlinOctaves) < 0;
} else {
	tmp = false;
}
if(tmp) {
	return false;
}
```

- `adjustScale(50,1)` sets `this.repeat = 50*1 | 0 = 50` (`188201-188204`).
- `persist` defaults to 0.5 and `lacunarity` to 2.0.
- The noise seed is `li.seed + rule.perlinSeed`, an exact integer sum (both < 1e7).
- The noise is evaluated at the unflipped `(cx, cy)`, so flips don't matter here either.

### Constructor, `renderer_fixed.js:188157-188168`

```js
var hxd_Perlin = function() {
	this.repeat = 2147483647;
	this.gradients = [];
	var _g = 0;
	while(_g < 256) {
		var i = _g++;
		this.gradients.push(hxd_Perlin.GRADIENTS[i * 3] * 2.12);
		this.gradients.push(hxd_Perlin.GRADIENTS[i * 3 + 1] * 2.12);
		this.gradients.push(hxd_Perlin.GRADIENTS[i * 3 + 2] * 2.12);
		this.gradients.push(0);
	}
};
```

`hxd_Perlin.GRADIENTS` (line 315563) has 768 f64 literals; they're reproduced in the appendix. Each is multiplied by `2.12` in f64. Rust parses decimal literals with correct rounding, same as JS. Only gradient components 0 and 1 (x, y) are used.

### `perlin()`, `renderer_fixed.js:188380-188438`

The relevant body:

```js
,perlin: function(seed,x,y,octaves,persist,lacunarity) {
	if(lacunarity == null) { lacunarity = 2.0; }
	if(persist == null) { persist = 0.5; }
	var v = 0.;
	var k = 1.;
	var sum = 0.;
	var _g = 0;
	var _g1 = octaves;
	while(_g < _g1) {
		var i = _g++;
		var seed1 = seed + i;
		var ix = x | 0;
		var a = x - ix;
		var a2 = a * a;
		var xs = a2 * a * (6.0 * a2 - 15.0 * a + 10.0);
		var iy = y | 0;
		var a1 = y - iy;
		var a21 = a1 * a1;
		var ys = a21 * a1 * (6.0 * a21 - 15.0 * a1 + 10.0);
		var index = seed1 * 1013 + ix % this.repeat * 1619 + iy % this.repeat * 31337;
		index = (index ^ index >>> 8) & 255;
		var gx = this.gradients[index << 2];
		var gy = this.gradients[(index << 2) + 1];
		var ga = gx * (x - ix) + gy * (y - iy);
		var ix1 = ix + 1;
		var index1 = seed1 * 1013 + ix1 % this.repeat * 1619 + iy % this.repeat * 31337;
		index1 = (index1 ^ index1 >>> 8) & 255;
		var gx1 = this.gradients[index1 << 2];
		var gy1 = this.gradients[(index1 << 2) + 1];
		var gb = gx1 * (x - ix1) + gy1 * (y - iy);
		var iy1 = iy + 1;
		var index2 = seed1 * 1013 + ix % this.repeat * 1619 + iy1 % this.repeat * 31337;
		index2 = (index2 ^ index2 >>> 8) & 255;
		var gx2 = this.gradients[index2 << 2];
		var gy2 = this.gradients[(index2 << 2) + 1];
		var gc = gx2 * (x - ix) + gy2 * (y - iy1);
		var ix2 = ix + 1;
		var iy2 = iy + 1;
		var index3 = seed1 * 1013 + ix2 % this.repeat * 1619 + iy2 % this.repeat * 31337;
		index3 = (index3 ^ index3 >>> 8) & 255;
		var gx3 = this.gradients[index3 << 2];
		var gy3 = this.gradients[(index3 << 2) + 1];
		var gd = gx3 * (x - ix2) + gy3 * (y - iy2);
		var a3 = ga + xs * (gb - ga);
		v += (a3 + ys * (gc + xs * (gd - gc) - a3)) * k;
		sum += k;
		k *= persist;
		x *= lacunarity;
		y *= lacunarity;
	}
	if(this.normalize) {
		v /= sum;
	}
	return v;
}
```

Rust port. This was verified **bit-for-bit** (f64 `==`) against the JS on 20,000 random (seed, cx, cy, scale ∈ {0.2, 0.1, 0.05, 0.37, 1, 0.013}, octaves 1..4) samples:

```rust
const REPEAT: i64 = 50;
fn grad_index(seed1: i64, ix: i64, iy: i64) -> usize {
    // JS: seed1*1013 + ix%50*1619 + iy%50*31337, all in f64 but exact (< 2^53).
    // `%` truncates (JS and Rust agree, including for negative ix).
    let n: i64 = seed1 * 1013 + (ix % REPEAT) * 1619 + (iy % REPEAT) * 31337;
    let u: u32 = n as u32;                // ToInt32/ToUint32 share the low 32 bits
    (((u ^ (u >> 8)) & 255) as usize) << 2 // only bits 0..7 survive the & 255, so the
                                          // signed/unsigned shift difference is irrelevant
}
fn ldtk_perlin(seed: i64, mut x: f64, mut y: f64, octaves: i32, grads: &[f64; 1024]) -> f64 {
    let (mut v, mut k, mut sum) = (0.0f64, 1.0f64, 0.0f64);
    for i in 0..octaves {                 // octaves <= 0: loop skipped, v = 0/0 = NaN
        let s1 = seed + i as i64;
        let ix = x as i32 as i64;         // JS `x|0`: truncate toward zero (fine for |x| < 2^31)
        let iy = y as i32 as i64;
        let a = x - ix as f64;  let a2 = a*a;  let xs = a2*a*(6.0*a2 - 15.0*a + 10.0);
        let b = y - iy as f64;  let b2 = b*b;  let ys = b2*b*(6.0*b2 - 15.0*b + 10.0);
        let j = grad_index(s1, ix,   iy  ); let ga = grads[j]*(x - ix as f64)     + grads[j+1]*(y - iy as f64);
        let j = grad_index(s1, ix+1, iy  ); let gb = grads[j]*(x - (ix+1) as f64) + grads[j+1]*(y - iy as f64);
        let j = grad_index(s1, ix,   iy+1); let gc = grads[j]*(x - ix as f64)     + grads[j+1]*(y - (iy+1) as f64);
        let j = grad_index(s1, ix+1, iy+1); let gd = grads[j]*(x - (ix+1) as f64) + grads[j+1]*(y - (iy+1) as f64);
        let a3 = ga + xs*(gb - ga);
        v += (a3 + ys*(gc + xs*(gd - gc) - a3)) * k;   // keep this exact operation order
        sum += k; k *= 0.5; x *= 2.0; y *= 2.0;
    }
    v / sum                                           // normalize = true
}
// gate: reject iff ldtk_perlin(li.seed + perlinSeed, cx as f64 * perlinScale, cy as f64 * perlinScale, perlinOctaves) < 0.0
```

Notes:

- **Input multiply.** `cx * perlinScale` is a plain f64 multiply, e.g. `3*0.2 = 0.6000000000000001`. Do it the same way.
- **Threshold.** The gate rejects only on `< 0`. Exactly `0.0` passes, and `NaN` passes too.
- **Lattice points give exactly 0.** At integer points, each corner gradient is multiplied by a zero distance. So with `perlinScale = 1` (or any scale where `cx*scale` and `cy*scale` are both integers) the noise is always exactly 0 and the gate never rejects. Cell (0,0) always passes.
- **The periodicity is fake.** `repeat` only wraps the lattice index *inside the hash*. `ix+1` is computed before the `%`, so the noise is not truly periodic.
- **Negative inputs.** A negative `perlinScale` gives `x|0` truncation (not floor) and negative `%`; the Rust above reproduces both.
- **No FMA.** Use no fused multiply-add. Rust doesn't contract FMAs automatically; keep it that way, e.g. no `mul_add`.

Test vectors produced by running the bundle's own `hxd_Perlin` (normalize = true, repeat = 50):

| seed (li.seed+perlinSeed) | cx | cy | scale | octaves | perlin() | passes |
|---|---|---|---|---|---|---|
| 8888888 | 3 | 5 | 0.2 | 2 | -0.2532768516334933 | no |
| 100 | 0 | 0 | 0.2 | 2 | 0 | yes |
| 100 | 1 | 0 | 0.2 | 2 | -0.09903012645034666 | no |
| 5000000 | 17 | 33 | 0.1 | 3 | -0.2178945449205813 | no |
| 42 | 250 | 4 | 0.05 | 1 | -0.28712357131775995 | no |
| 9999999 | 12 | 12 | 1 | 4 | 0 | yes |
| 1234 | 1 | 7 | 0.2 | 2 | 0.33326698337410554 | yes |
| 1234 | 4 | 7 | 0.2 | 2 | 0.07144019015908505 | yes |
| 1234 | 10 | 7 | 0.2 | 2 | 0.1716934174296749 | yes |
| 1234 | 13 | 7 | 0.2 | 2 | 0.16160192336807072 | yes |

## 5. Pattern matching

### Order of checks for one (rule, cell, flip)

From `applyAllRulesAt`, `renderer_fixed.js:58940-58955`, then `matches`:

1. `isRelevantInLayerAt(source, x, y)`. A pruning step, but it **can change results** (see 5.4).
2. Modulo/checker gates (section 6).
3. `matches(li, source, x, y, dirX, dirY)`, which runs in this order:
   1. `tileRectsIds.length == 0` → false;
   2. chance;
   3. perlin;
   4. pattern cells.

Steps 1, 2 and the chance/perlin parts of 3 don't depend on the flip, so they're effectively evaluated once per cell.

### 5.1 Flip attempts

`renderer_fixed.js:58954-58955, 59393-59395, 59833-59835, 60273-60275, 60713`:

```js
var matched = false;
if(r.matches(_gthis,source,x,y)) {             // dirX=1, dirY=1 -> place with flips=0
	... matched = true;
}
if((!matched || !r.breakOnMatch) && r.flipX && r.matches(_gthis,source,x,y,-1)) {        // flips=1
	... matched = true;
}
if((!matched || !r.breakOnMatch) && r.flipY && r.matches(_gthis,source,x,y,1,-1)) {      // flips=2
	... matched = true;
}
if((!matched || !r.breakOnMatch) && r.flipX && r.flipY && r.matches(_gthis,source,x,y,-1,-1)) { // flips=3
	... matched = true;
}
```

- With `breakOnMatch`, the first matching attempt wins.
- Without it, every matching attempt places its own tiles, stacked at the same cell.
- `matched = true` is set **even if** the tiles were suppressed by `autoTilesKilledByOtherLayerUid` (see 7.4).

### 5.2 Pattern cell loop, `renderer_fixed.js:38851-39064`

Condensed: the inlined ceil and `cWid`/`cHei` computations are replaced by names, and the per-cell logic is verbatim.

```js
var value = 0;
var valueInf = null;
var radius = this.size / 2 | 0;
... for px in 0..size { for py in 0..size {
	var coordId = px + py * this.size;
	if(this.pattern[coordId] == 0) { continue; }
	var cx1 = cx + dirX * (px - radius);
	var cy1 = cy + dirY * (py - radius);
	// in-bounds test against the SOURCE layer: cx1>=0 && cx1<cWid(source) && cy1>=0 && cy1<cHei(source)
	if(inBounds) {
		value5 = !source.intGrid.h.hasOwnProperty(cx2 + cy2 * key);   // key = cWid(source)
		if(value5) { value = 0; } else { value = source.intGrid.h[cx2 + cy2 * key1]; }
	} else {
		value = this.outOfBoundsValue;
	}
	if(value == null) {
		return false;
	}
	var x6 = this.pattern[coordId];
	if((x6 < 0 ? -x6 : x6) == Const.AUTO_LAYER_ANYTHING) {
		if(this.pattern[coordId] > 0 && value == 0) { return false; }
		if(this.pattern[coordId] < 0 && value != 0) { return false; }
	} else {
		var x7 = this.pattern[coordId];
		if((x7 < 0 ? -x7 : x7) > 999) {
			// valueInf = first entry of source layerDef.intGridValues with v.value == value, else null
			if(this.pattern[coordId] > 0 && (valueInf == null || valueInf.groupUid != (this.pattern[coordId] / 1000 | 0) - 1)) { return false; }
			if(this.pattern[coordId] < 0 && (valueInf != null && valueInf.groupUid == (-this.pattern[coordId] / 1000 | 0) - 1)) { return false; }
		} else {
			if(this.pattern[coordId] > 0 && value != this.pattern[coordId]) { return false; }
			if(this.pattern[coordId] < 0 && value == -this.pattern[coordId]) { return false; }
		}
	}
}}
return true;
```

Pseudocode:

```rust
// pattern: Vec<i32> of len size*size, row-major: pattern[px + py*size]
// radius = size / 2 (size is odd, 1..=9)
fn pattern_matches(rule, source, cx, cy, dir_x: i32, dir_y: i32) -> bool {
    for px in 0..size { for py in 0..size {              // column-major; order is irrelevant
        let p = rule.pattern[px + py*size];              // (pure AND, no side effects)
        if p == 0 { continue; }
        let sx = cx + dir_x * (px as i32 - radius);      // flip mirrors the sampling offset
        let sy = cy + dir_y * (py as i32 - radius);      // around the anchor
        let value: Option<i32> =
            if 0 <= sx && sx < cwid(source) && 0 <= sy && sy < chei(source) {
                Some(source.int_grid.get(sx + sy*cwid(source)).copied().unwrap_or(0))
            } else { rule.out_of_bounds_value };         // Option<i32>, JSON null -> None
        let Some(value) = value else { return false };   // null OOB fails EVERY non-zero cell,
                                                         // including negative/"nothing" cells
        let ap = p.abs();
        if ap == 1000001 {                               // ANYTHING
            if p > 0 && value == 0 { return false; }
            if p < 0 && value != 0 { return false; }
        } else if ap > 999 {                             // group code
            let g = ap / 1000 - 1;                       // integer division (ap > 0)
            let vg: Option<i32> = source_def.int_grid_values.iter()
                .find(|iv| iv.value == value).map(|iv| iv.group_uid);  // first match; group_uid defaults 0
            if p > 0 && vg != Some(g) { return false; }
            if p < 0 && vg == Some(g) { return false; }
        } else {
            if p > 0 && value != p  { return false; }
            if p < 0 && value == -p { return false; }
        }
    }}
    true
}
```

**Group code encoding.** Pattern value `(groupUid + 1) * 1000`, positive for "is in group", negative for "is not in group". Ungrouped values have `groupUid = 0`, so code 1000 means "any ungrouped value". `1000001` (ANYTHING) is excluded from the group branch. Value 0 (empty) has no `intGridValues` entry, so `vg = None`: positive group cells fail on it and negative group cells pass.

**Where cells come from.**

- **Loading.** `intGrid` is loaded from `intGridCsv` at `49666-49673`, and **every CSV entry `>= 0` is stored, zeros included**. A stored 0 behaves like an absent cell.
- **Which layer.** `source` is the layer instance itself for an IntGrid layer, or else the level's instance of `ld.autoSourceLayerDefUid` (`58789-58806`).
- **Two grids.** Bounds and sampling use **the source's** `cWid`/`cHei`. The anchor loop (section 9) uses **the target's**.

**Out-of-bounds.**

- If `outOfBoundsValue` is null, any non-zero pattern cell that lands outside the level makes the whole match fail.
- If it's an int, it stands in for the missing value and goes through all the comparisons above. For example, OOB = 0 makes ANYTHING fail.
- `tidy()` (`39231-39257`) nulls out an OOB value that isn't a defined IntGrid value of the source layer def. Saved JSON is post-tidy, since `toJson` calls `tidy`.

### 5.3 Flip/tidy normalization

`AutoLayerRuleDef.tidy` runs from `toJson` (`38621`). It clears `flipX` if the pattern is X-symmetric (`isSymetricX`, `38549`) and `flipY` if it's Y-symmetric. It also trims all-zero borders, shrinking `size` by 2 at a time. Saved JSON is post-tidy, so use the JSON as-is. But baked tiles may predate the tidy; see "Where parity can break".

### 5.4 Pruning: `isRelevantInLayerAt`, `renderer_fixed.js:38759-38808`

```js
,isRelevantInLayerAt: function(sourceLi,cx,cy) {
	var _g = 0;
	var _g1 = this.explicitlyRequiredValues;
	while(_g < _g1.length) {
		var v = _g1[_g];
		++_g;
		if(!sourceLi.layerIntGridUseCount.h.hasOwnProperty(v)) {
			return false;
		} else if(this.size == 1 && !(sourceLi.areaIntGridUseCount.h.hasOwnProperty(v) && sourceLi.areaIntGridUseCount.h[v].h[(cx / sourceLi.intGridAreaSize | 0) + (cy / sourceLi.intGridAreaSize | 0) * 10000] > 0)) {
			return false;
		} else {
			// size > 1: return false only if ALL FOUR corner areas lack v:
			//   ((cx - r)/10|0, (cy - r)/10|0), ((cx + r)/10|0, (cy - r)/10|0),
			//   ((cx + r)/10|0, (cy + r)/10|0), ((cx - r)/10|0, (cy + r)/10|0)
			// with r = (size <= 1 ? 1 : size * 0.5 | 0)
			...
			if(tmp) { return false; }
		}
	}
	return true;
}
```

Supporting definitions:

- **Required values.** `explicitlyRequiredValues` (`38501-38512`) is every distinct pattern value with `v > 0 && v != 1000001`. Group codes are included; negative values are not.
- **Counters.** `layerIntGridUseCount` and `areaIntGridUseCount` are rebuilt on load by `recountAllIntGridValues` (`49960`) through `increaseAreaIntGridValueCount` (`50283-50317`):

```js
if(iv == 0 || iv == null) { return; }
...
var cid = (cx / this.intGridAreaSize | 0) + (cy / this.intGridAreaSize | 0) * 10000;   // intGridAreaSize = 10
... areaCountMap.h[cid] += 1 ... layerIntGridUseCount.h[iv] += 1 ...
if(iv < 1000) {
	var groupUid = (...).getIntGridGroupUidFromValue(iv);   // -1 if iv isn't a defined value
	if(groupUid >= 0) {
		this.increaseAreaIntGridValueCount(groupUid < 0 ? -1 : (groupUid + 1) * 1000,cx,cy);
	}
}
```

So a group code is counted for every non-zero cell whose value is *defined* in the layer def and below 1000.

**Does pruning only skip work? Almost, but not quite.**

- **In bounds, it's exact.** With `r <= 4` and 10x10 areas, the window `[cx-r, cx+r]` spans at most 2 areas per axis, and the four corners hit exactly those areas. Negative coordinates truncate to area 0 (`-3/10|0 == 0`), which is also the area of the first in-bounds column. So if any in-bounds window cell holds `v` (or a value in group `v`), at least one corner area counts it and pruning passes.
- **Out of bounds, it can drop real matches.** Pruning rejects whenever `v` is absent from the whole source layer, or from all four corner areas, *even if the out-of-bounds value would satisfy that pattern cell*.
  - Example: a rule requires 1 on its left edge, `outOfBoundsValue = 1`, and the layer contains no 1 at all. `matches()` alone would accept at `cx = 0`, but LDtk never places the tile.

**So implement the prune literally:** same 10x10 area keys, `ax + ay*10000` with truncating division, and the same four-corner test.

## 6. Modulo and checker gates

`renderer_fixed.js:58950-58953` (identical in `applyRuleAt`, `56686-56697`):

```js
if(!(r.checker != ldtk_AutoLayerRuleCheckerMode.Vertical && (y - r.yOffset) % r.yModulo != 0)) {
	if(!(r.checker == ldtk_AutoLayerRuleCheckerMode.Vertical && (y + (x / r.xModulo | 0) % 2) % r.yModulo != 0)) {
		if(!(r.checker != ldtk_AutoLayerRuleCheckerMode.Horizontal && (x - r.xOffset) % r.xModulo != 0)) {
			if(!(r.checker == ldtk_AutoLayerRuleCheckerMode.Horizontal && (x + (y / r.yModulo | 0) % 2) % r.xModulo != 0)) {
```

```rust
// x, y = anchor cell (>= 0). Checker: None | Horizontal | Vertical (JSON strings)
fn modulo_ok(r, x: i32, y: i32) -> bool {
    if r.checker != Vertical   && (y - r.y_offset) % r.y_modulo != 0 { return false; }
    if r.checker == Vertical   && (y + (x / r.x_modulo) % 2) % r.y_modulo != 0 { return false; }
    if r.checker != Horizontal && (x - r.x_offset) % r.x_modulo != 0 { return false; }
    if r.checker == Horizontal && (x + (y / r.y_modulo) % 2) % r.x_modulo != 0 { return false; }
    true
}
```

- **Negative values.** `(y - yOffset)` can be negative when `yOffset > y`. JS `%` then returns a negative or zero remainder, but the test is only `!= 0`, so the sign convention doesn't matter. Rust `%` gives the same zero/non-zero answer.
- **Checker mode drops the offset on its own axis.** In Vertical checker mode `yOffset` is ignored (the Y test becomes the checker test). In Horizontal mode `xOffset` is ignored. The other axis's plain modulo test still applies.
- **Zero moduli:**
  - `% 0` is `NaN` in JS, and `NaN != 0` is true, so modulo 0 **rejects every cell**. Guard against it in Rust; don't divide.
  - `x / 0 | 0` (checker with `xModulo = 0`) is `Infinity | 0 = 0`.
  - `tidy()` (`39125-39208`) resets `checker` when the matching modulo is 1.

## 7. Placing tiles (`addRuleTilesAt`, inlined at 59019-59392 etc.; standalone copy at 56197-56635)

### 7.1 Tile list and `src`

`renderer_fixed.js:56570-56633`:

```js
var value1 = cx * (layerDef).gridSize + (stampInfos == null ? 0 : stampInfos.h[tid].xOff) + r.getXOffsetForCoord(_gthis.seed,cx,cy,flips);
var value2 = cy * (layerDef).gridSize + (stampInfos == null ? 0 : stampInfos.h[tid].yOff) + r.getYOffsetForCoord(_gthis.seed,cx,cy,flips);
...
result[i] = { x : value1, y : value2, srcX : td.padding + (tid - value3 * (tid / value4 | 0)) * (td.tileGridSize + td.spacing), srcY : td.padding + (tid / value5 | 0) * (td.tileGridSize + td.spacing), tid : tid, flips : flips, a : r.alpha};
...
var value1 = value.concat(result);
this1.h[key1] = value1;     // autoTilesCache[r.uid][cx + cy*cWid(li)] (append)
```

Here `value3`, `value4` and `value5` are all `tdCWid(td)`, and `td = li.getTilesetDef()`. That is `li.overrideTilesetUid` if set, otherwise `ld.tilesetDefUid` (`50376-50428`).

```rust
let g = layer_def.grid_size;                     // LAYER grid size, not tileset grid size
let tdw = td_cwid(td);                           // ceil((pxWid - 2*padding)/(tileGridSize+spacing)), or 0
for &tid in alt {                                // alt = pick_alternative(...), in order
    let (sx, sy) = stamp.map_or((0,0), |s| s[&tid]);
    let px = cx*g + sx + x_offset(r, li.seed, cx, cy, flips);
    let py = cy*g + sy + y_offset(r, li.seed, cx, cy, flips);
    let col = if tdw == 0 { tid } else { tid - tdw*(tid / tdw) };   // JS: tid - 0*(Infinity|0) = tid
    let row = if tdw == 0 { 0 }   else { tid / tdw };
    let src = [td.padding + col*(td.tile_grid_size + td.spacing),
               td.padding + row*(td.tile_grid_size + td.spacing)];
    push(Tile { px: [px, py], src, f: flips, t: tid, anchor: cx + cy*cwid(li), rule: r.uid });
}
```

- `px` does **not** include the layer's `pxOffsetX/Y`. Those are exported separately as `__pxTotalOffset*`.
- `src` uses the tileset's `padding`/`spacing`/`tileGridSize`, computed when the tile is placed. `t` is the raw tile id.

### 7.2 Stamp offsets (`tileMode == "Stamp"` and `td != null`; Single mode gets `stampInfos = null` → 0)

Inlined at `56316-56495`, same as `getRuleStampRenderInfos` (`50854-51038`):

```js
var top = 99999; var left = 99999; var right = 0; var bottom = 0;
for tid in tileRectIds:
	row = tid / tdCWid | 0;  if(top >= row) top = row;  if(bottom <= row) bottom = row;
	col = tid - tdCWid * (tid / tdCWid | 0);  if(left >= col) left = col;  if(right <= col) right = col;
...
out.h[tid] = { xOff : ((tid - x * (tid / x2 | 0) - left - r.pivotX * (right - left) + (ld).tilePivotX) * (ld).gridSize | 0) * ((flips & 1) != 0 ? -1 : 1), yOff : (((tid / x4 | 0) - top - r.pivotY * (bottom - top) + (ld).tilePivotY) * (ld).gridSize | 0) * ((flips & 2) != 0 ? -1 : 1)};
```

```rust
// all in f64, left-to-right exactly as written, then JS `|0` = truncate toward zero
let fx: f64 = (col - left) as f64 - r.pivot_x * ((right - left) as f64) + ld.tile_pivot_x;
let xoff = ((fx * ld.grid_size as f64).trunc() as i32) * (if flips & 1 != 0 { -1 } else { 1 });
let fy: f64 = (row - top) as f64 - r.pivot_y * ((bottom - top) as f64) + ld.tile_pivot_y;
let yoff = ((fy * ld.grid_size as f64).trunc() as i32) * (if flips & 2 != 0 { -1 } else { 1 });
```

Precisely: JS evaluates `tid - x*(tid/x2|0)` (that's `col`), then `- left`, then `- r.pivotX*(right-left)`, then `+ tilePivotX`, all in f64, before `* gridSize` and `| 0`.

- **Truncation, not floor.** E.g. `-7.5 → -7`. It happens before the flip sign is applied.
- **Flipped stamps mirror their layout.** Each tile's offset from the anchor is negated, and each tile also carries `f` so the renderer flips its pixels. Stamp tiles therefore land at mirrored positions around the pivot point.
- **`tilePivotX/Y` only affects stamps.** It's read from the layer def JSON and clamped to [0,1] on load (`41443-41446`). Single-mode tiles ignore it entirely.
- **`rule.pivotX/Y`** is read as a float and not clamped (`38473-38474`).
- **The bounding box uses tileset grid coordinates**, while the offset is scaled by the **layer** `gridSize`.
- **Repeated tids share one offset**, because `out.h[tid]` is keyed by tid. Both copies are still placed.

### 7.3 `f` bits

`f = flips` is the attempt index: `0` = none, `1` = X, `2` = Y, `3` = X+Y. So bit 0 is flipX and bit 1 is flipY. It's the same value that is fed into the alternative/X-offset hashes and that negates offsets.

### 7.4 `autoTilesKilledByOtherLayerUid`

`renderer_fixed.js:58961-59065` and `56203-56307`. If the layer def sets `autoTilesKilledByOtherLayerUid`, look up that layer's instance in the same level. If its `gridTiles` has a **non-empty** stack at key `cx + cy*cWid(otherLayer)`, using the anchor's `cx, cy` with the other layer's own `cWid` and bounds check, then **no tiles are placed and no cache key is created**. `matched` is still set to true, so `breakOnMatch` still blocks the flip attempts.

### 7.5 Cache-key-without-tiles edge case

The cache key `autoTilesCache[r.uid][anchor]` is created (as `[]`) whenever `addRuleTilesAt` gets past the kill check, even if the chosen alternative is empty. An empty-array alternative therefore creates a key with zero tiles. That still counts as "has tiles here" for the break-on-match post-pass (section 8) and can lock the cell.

## 8. Break-on-match post-pass: `applyBreakOnMatchesArea`, `renderer_fixed.js:58574-58775`

It runs once at the end of `applyAllRulesAt` (`60726`) over the same clamped area. For a full rebuild that's the whole layer.

```js
var coordLocks_h = { };
var td = this.getTilesetDef();
for y in top..=bottom {             // row-major here (irrelevant, see below)
 for x in left..=right {
  for rg in ld.autoRuleGroups (forward) {
   if(this.isRuleGroupAppliedHere(rg)) {
    for r in rg.rules (forward) {
     if(r.active) {
      if(autoTilesCache has r.uid && autoTilesCache[r.uid] has key x + y*cWid) {
        if(coordLocks_h.hasOwnProperty(x + y * key1)) {
            _gthis.autoTilesCache.h[r.uid].remove(x + y * key2);          // delete ALL of this rule's tiles anchored here
        } else if(r.breakOnMatch) {
            coordLocks_h[x + y * key3] = true;
        } else if(!(r.tileRandomXMin != 0 || r.tileRandomXMax != 0 || r.tileRandomYMin != 0 || r.tileRandomYMax != 0 || r.tileXOffset != 0 || r.tileYOffset != 0) && r.alpha >= 1) {
            for t10 in autoTilesCache[r.uid][x + y*cWid] {
                if(td.opaqueTiles != null && td.opaqueTiles[t10.tid] == true) {
                    coordLocks_h[x + y * key5] = true;
                    break;
                }
            }
        }
      }
     }
    }
   }
  }
 }
}
```

The rule, in words:

- **Scope.** For each anchor cell, walk the applied groups and active rules in **forward** order (top of the list first).
- **First rule with tiles at the cell decides.** If it is `breakOnMatch`, it locks the cell. If it is non-breakOnMatch but has **no position offset of any kind**, has `alpha >= 1`, and **at least one** of its tiles at that anchor is opaque, it also locks the cell.
- **After the lock.** Every later rule that has tiles at a locked cell loses **all** its tiles at that anchor: every flip variant and every stamp tile. That includes later `breakOnMatch` rules.
- **What isn't checked.** Stamp offsets and pivots are not considered: a stamp counts as "opaque at the anchor" if any tile in it is opaque, even one drawn several cells away. Locks are keyed purely by **anchor** coordId, not by where tiles are drawn.

**Why order is safe.** In the main pass each rule only writes `autoTilesCache[r.uid][anchor]` and only reads `intGrid` (plus the other layer's `gridTiles`). Nothing in it depends on other rules. All cross-rule interaction happens in this post-pass, and its lock decision for a cell depends only on that cell. So both the main pass's rule-major, column-major loop order and this pass's row-major order are irrelevant to the result.

### Opaque tiles

`td.opaqueTiles` is a `bool[]` of length `tdCWid * tdCHei`, indexed by tid. An index out of range gives `undefined`, which counts as not opaque. If `opaqueTiles` is null, nothing is opaque and no opacity locks happen. There are two sources:

1. **From the `.ldtk` file:** `defs.tilesets[].cachedPixelData.opaqueTiles`, a string of `'0'`/`'1'` chars, one per tid (`42606-42616`). It's used only if its length equals `tdCWid*tdCHei` and `averageColors.length % 4 == 0`.
   ```js
   td.opaqueTiles[i] = HxOverrides.cca(data.opaqueTiles,i) == one;   // one = 49 ('1')
   ```
   On initial load, if this cache is valid and the image loaded OK, LDtk uses it and **does not** recompute (`233700-233713`). **Prefer this string for parity**, because it's exactly what LDtk had in memory.
2. **From image pixels:** `_parseTilePixels`, `44694-44783`:
   ```js
   this.opaqueTiles[tid] = true;
   ...
   var tx2 = this.padding + (tid - tx * (tid / tx1 | 0)) * (this.tileGridSize + this.spacing);
   var ty1 = this.padding + (tid / ty | 0) * (this.tileGridSize + this.spacing);
   if(tx2 + this.tileGridSize <= this.pxWid && ty1 + this.tileGridSize <= this.pxHei) {
   	... for each pixel in the tileGridSize x tileGridSize rect:
   		pixel = img.pixels.getPixel(px,py);
   		if(this.opaqueTiles[tid] != false && (pixel >> 24 & 255) / 255 < 1) {
   			this.opaqueTiles[tid] = false;
   		}
   }
   ```
   - **Threshold.** A tile is opaque iff **every** pixel has alpha byte `== 255`. `(a/255) < 1` is true for any `a <= 254`.
   - **Quirk.** A tile whose rect runs past the image edge (possible because `tdCWid` uses ceil) is never scanned, so it stays **opaque = true**.

## 9. Main-pass bounds, output ordering and serialization

### Main pass: `applyAllRulesAt(cx, cy, wid, hei)`, `58880-58955`

```js
var maxRadius = Const.MAX_AUTO_PATTERN_SIZE * 0.5 | 0;          // = 4
left = max(0, cx - maxRadius);  right = min(cWid(li) - 1, cx + wid - 1 + maxRadius);
top  = max(0, cy - maxRadius);  bottom = min(cHei(li) - 1, cy + hei - 1 + maxRadius);
for rg in ld.autoRuleGroups (forward) if isRuleGroupAppliedHere(rg):
  for r in rg.rules (forward) if r.active:
    clearAutoTilesCacheRect(r, left, top, right-left+1, bottom-top+1);
    for x in left..=right { for y in top..=bottom { ...section 5/6/7... } }
applyBreakOnMatchesArea(left, top, right-left+1, bottom-top+1);
```

The full rebuild (`applyAllRules`, `60728`) clears all caches and calls this with `(0, 0, cWid, cHei)`. It does nothing unless `autoLayerRulesCanBeUsed()` holds (`42021-42032`):

- the layer is IntGrid with a tileset, or AutoLayer;
- `tilesetDefUid != null`;
- an AutoLayer has an `autoSourceLayerDefUid`.

### Serialization: `LayerInstance.toJson`, `renderer_fixed.js:50654-50692`

```js
var arr = [];
if(this.autoTilesCache != null) {
	var td1 = this.getTilesetDef();
	var _this1 = (layerDef);
	var ruleGroupIdx = _this1.autoRuleGroups.length - 1;
	while(ruleGroupIdx >= 0) {
		if(this.isRuleGroupAppliedHere(_this1.autoRuleGroups[ruleGroupIdx])) {
			var rg = _this1.autoRuleGroups[ruleGroupIdx];
			var ruleIdx = rg.rules.length - 1;
			while(ruleIdx >= 0) {
				if(rg.rules[ruleIdx].active) {
					var r = rg.rules[ruleIdx];
					if(_gthis.autoTilesCache.h.hasOwnProperty(r.uid)) {
						var map = _gthis.autoTilesCache.h[r.uid];
						var allTiles_keys = map.keys();
						while(allTiles_keys.hasNext()) {
							var key = allTiles_keys.next();
							...
							arr.push({ px : [tileInfos.x,tileInfos.y], src : [tileInfos.srcX,tileInfos.srcY], f : tileInfos.flips, t : tileInfos.tid, d : [r.uid,allTiles_key], a : r.alpha});
						}
					}
				}
				--ruleIdx;
			}
		}
		--ruleGroupIdx;
	}
}
```

`IntMap.keys()`:

```js
,keys: function() {
	var a = [];
	for( var key in this.h ) if(this.h.hasOwnProperty(key)) a.push(+key);
	return new haxe_iterators_ArrayIterator(a);
}
```

Output order, outermost loop first:

1. **Groups in reverse** definition order, skipping those not applied here (section 10).
2. **Rules in reverse** order within the group, skipping `active == false`.
3. **Anchor coordIds ascending.** `h` is a plain JS object and coordIds are non-negative integer keys, and JS orders such keys ascending no matter when they were inserted.
4. **Within one anchor, insertion order:**
   - flip attempts in order 0, X(1), Y(2), XY(3);
   - within an attempt, the order of tids in the chosen alternative.

Fields:

- `px: [x, y]`: integer pixels relative to the layer, not including `pxOffset`.
- `src: [srcX, srcY]`.
- `f`: 0..3.
- `t`: tid.
- `d: [ruleUid, anchorCoordId]`, where `anchorCoordId = cx + cy*cWid(li)` is the **matched cell**, not the cell the tile is drawn in.
- `a: r.alpha`. This is the rule's alpha *at save time*; the cached `a` is ignored.

**No deduplication anywhere.**

- Stacked flip variants, repeated tids, and tiles from different rules at the same pixel are all emitted.
- The only removals are the post-pass deletions (section 8) and the skipping of inactive or non-applied rules at write time.
- Rules that sit in non-applied groups, or are inactive, may still have stale cache entries. They are simply not written.

**Number formatting.** LDtk writes the file with `dn.data.JsonPretty`.

- Integers are written as ints; `-0` (possible in `px` when a flip negates a zero offset at `cx = 0`) prints as `0`.
- `a` prints as an int when it's integral (`1`), otherwise as JS shortest round-trip (`0.5`).

## 10. Optional groups and biomes: `isRuleGroupAppliedHere`, `renderer_fixed.js:58461-58516`

```js
,isRuleGroupAppliedHere: function(rg) {
	if(rg.active && rg.requiredBiomeValues.length > 0) {
		var fi = (level).getFieldInstanceByUid((layerDef).biomeFieldUid,false);
		if(fi != null) {
			switch(rg.biomeRequirementMode) {
			case 0:
				var _g1 = fi._project.defs.getFieldDef(fi.defUid).isArray ? fi.internalValues.length : 1;
				while(_g < _g1) {
					var idx = _g++;
					for bid in rg.requiredBiomeValues:
						if(fi.getEnumValue(idx) == bid) { return true; }
				}
				return false;
			case 1:
				var matches = 0;
				for bid in rg.requiredBiomeValues:
					for arrayIdx in 0..(isArray ? fi.internalValues.length : 1):
						if(fi.getEnumValue(arrayIdx) == bid) { ++matches; break; }
				return matches >= rg.requiredBiomeValues.length;
			default:
				return false;
			}
		}
	}
	if(!(rg.active && !rg.isOptional)) {
		return this.optionalRules.h.hasOwnProperty(rg.uid);
	} else {
		return true;
	}
}
```

```rust
fn group_applied(rg, li, level, ld) -> bool {
    if rg.active && !rg.required_biome_values.is_empty() {
        if let Some(fi) = level.field_instance(ld.biome_field_uid) {  // None if biomeFieldUid null or missing
            let n = if fi.def.is_array { fi.values.len() } else { 1 };
            let val = |i| fi.enum_value(i);  // internalValues[i], or the field def's default enum id
                                             // if null/undefined; may be None
            return match rg.biome_requirement_mode {
                0 => (0..n).any(|i| rg.required_biome_values.iter().any(|b| val(i).as_deref() == Some(b))),
                1 => rg.required_biome_values.iter().filter(|b| (0..n).any(|i| val(i).as_deref() == Some(*b))).count()
                         >= rg.required_biome_values.len(),
                _ => false,
            };   // NOTE: biome result short-circuits; isOptional / optionalRules ignored
        }
    }
    if rg.active && !rg.is_optional { true } else { li.optional_rules.contains(&rg.uid) }
}
```

Surprises:

- **Biome groups ignore optional state.** When the group is active, has biome requirements, and the level has the biome field instance, the biome answer is final: `isOptional` and the layer's `optionalRules` are ignored.
- **Inactive groups can still apply.** If `rg.active == false` and `rg.uid` is in `li.optionalRules`, the group runs.
- **Mode "All" (1)** counts each required biome at most once.
- **Missing field instance.** If `biomeFieldUid` is null or the level lacks the field instance, the biome check falls through to the normal optional/active logic.
- **Default values.** `getEnumValue(idx)` (`49295-49311`) returns `internalValues[idx]` (`F_Enum` payload `.v`). If that slot is null/undefined it returns the field def's default (`getEnumDefault`, which itself is null if the default isn't a valid enum value). Note that `n = 1` for a non-array field even if `internalValues` is empty, so the default is used.

The same function gates the main pass, the post-pass, and serialization.

## 11. Layer instance seed and other randomness

- `li.seed`:
  - **New instance:** `Std.random(9999999)` (`49638`), i.e. `floor(Math.random()*9999999)`, in `[0, 9999998]`.
  - **Loading:** `li.seed = data_JsonTools.readInt(json.seed, Std.random(9999999))` (`49653`). It falls back to a random value only if `seed` is missing, or is a non-int32 / non-integer number `>= 32767` (`readInt`, which also truncates fractional values below that via `| 0`). Always use the JSON `seed`.
- `rule.perlinSeed`: `readInt(json.perlinSeed, Std.random(9999999))` (`38489`), same caveats.
- **Nothing else is random.** All per-cell randomness is the deterministic hash of `(li.seed, rule.uid, flips|1, cx, cy)` plus Perlin on `li.seed + perlinSeed`. The target layer's seed is used for everything, never the source layer's.

## 12. Inputs beyond `__cWid` / `__cHei` that affect the result

1. **Tileset pixels** (only through `opaqueTiles` in the post-pass). Use `cachedPixelData.opaqueTiles` from the file. If you must recompute, apply the alpha-255 rule and the "rect past image edge stays opaque" quirk.
2. **Tileset geometry.** `pxWid`, `pxHei`, `padding`, `spacing`, `tileGridSize` feed `tdCWid` (tid → col/row), `src`, stamp bounding boxes, and the size of `opaqueTiles`. If the tileset has no `relPath` and no `embedAtlas`, `tdCWid = 0` (see section 0).
3. **The layer instance's tileset override.** `overrideTilesetUid` beats `ld.tilesetDefUid`.
4. **Grid dimensions of each layer.**
   - Target grid: `ceil((level.pxWid - li.pxOffsetX)/ld.gridSize)`, from level `pxWid/pxHei`, layer `pxOffsetX/Y`, and layer `gridSize`. Used for anchor iteration and `d[1]`.
   - **Source** grid: computed the same way from the source layer, and used for pattern bounds and sampling. If the two differ, the bounds disagree.
5. **The `autoTilesKilledByOtherLayerUid` layer's `gridTiles`** (7.4).
6. **IntGrid value definitions** (`intGridValues[].groupUid`) for group-code matching and group counts, and the defined-value set (used by `tidy` for OOB and by the group counters).
7. **The level's biome enum field instance and the enum field def default** (section 10). The layer's `optionalRules` list.
8. **`intGridAreaSize = 10`** and the area key `ax + ay*10000` (5.4). Areas collide only for levels wider than 100,000 cells.

---

## Where parity can break

**Float and integer semantics**

1. **Hash multiply.** `(h ^ h>>13) * 1274126177` is an f64 multiply rounded to 53 bits, then reduced mod 2^32. Integer `wrapping_mul` / `Math.imul` semantics give different bits for almost every input. This is the single biggest trap.
2. **Chance threshold.** `roll >= chance * 100` must be compared in f64. `0.07*100 = 7.000000000000001` gives 8% instead of 7%; the same goes for 0.14, 0.28, 0.55 and 0.56.
3. **JSON float parsing.** `chance`, `pivotX/Y`, `tilePivotX/Y`, `perlinScale` and `alpha` must parse to the identical f64. With `serde_json`, enable the **`float_roundtrip`** feature. The default fast path can be off by 1 ulp, which can flip a chance threshold, a Perlin sign, or a stamp `trunc`.
4. **Perlin.** It is pure f64 with a fixed operation order. Keep the exact expression shapes in section 4. No FMA, no algebraic simplification such as rewriting the smoothstep, and don't use `f32`. `x|0` is truncation, not floor.
5. **Stamp offsets.** They are f64 math, then truncation toward zero (JS `|0`), then the sign flip. Using `floor` or `round` breaks negative or fractional pivots. Example: pivot 0.5 on a 2-wide stamp with an odd `gridSize`.
6. **Divide-by-zero cases.** These yield `NaN`/`Infinity` in JS where Rust would panic:
   - `xModulo`/`yModulo == 0` makes the rule never match;
   - `tileRandomMax - tileRandomMin + 1 == 0` makes the offset `NaN`, which JsonPretty writes as the invalid token `NaN`;
   - `tdCWid == 0` makes `tid/0|0 = 0`;
   - `perlinOctaves <= 0` makes the noise `NaN`, so the gate passes.
7. **`-0`.** It appears in `px` and serializes as `0`. Compare numerically, or normalize.
8. **The ceil idiom.** It equals `ceil` only for `0 < x < 2^31`. It's fine for real levels.

**Ordering and hash maps**

9. **Anchor order** relies on JS ordering integer-like object keys ascending. It holds because coordIds are always in `[0, cWid*cHei)`. No other output depends on map iteration order:
   - `stampInfos` is looked up by key;
   - `coordLocks` is membership-only;
   - `optionalRules` and the counters are membership/count-only.
10. **Flip attempt order and alternative tid order** determine the order of tiles within one anchor. Keep it as 0, X, Y, XY.

**Baked data that may not match a fresh computation** (LDtk doesn't recompute on save)

11. **Incremental updates.** LDtk updates `autoTilesCache` incrementally around edited cells (a 4-cell margin). Some changes do *not* necessarily trigger a recompute:
    - edits to the kill-layer's tiles;
    - tileset image or opacity changes;
    - biome field edits;
    - rule `tidy()` at save (`flipX` cleared on a symmetric pattern, border trim, OOB value nulled, checker reset).

    If the baked file was produced before such a change, a from-scratch Rust result can legitimately differ. Verify suspected stale layers by re-saving in LDtk with "rebuild all auto-layers".
12. **3-decimal rounding on save.** `writeFloat` (`chance`, `pivotX/Y`, `perlinScale`) rounds to 3 decimals: `round_half_away(v*1000)/1000`. If the in-memory value had more precision when the tiles were baked, the reloaded value differs from the one used.
13. **Async pixel data.** `opaqueTiles` is built asynchronously, row by row, when the cache is invalid. Tiles baked while it was null (or partially filled) skipped the opacity locks. Prefer the saved `cachedPixelData` and expect occasional mismatches on freshly imported tilesets.
14. **Pruning is not a pure optimization** (5.4). Skipping it adds tiles wherever a positive requirement is met only by `outOfBoundsValue`.

---

## Test vectors: hash (from running the compiled JS)

`mix` is `ldtk_mix`. "X rand" is `getXOffsetForCoord` with `flips=1, tileXOffset=0, min=-2, max=3`, unflipped sign ignored. "Y rand" is the same for Y. Both draw the identical `h`; see section 3.

| seed | uid | cx | cy | h0 | mix(h0) | roll %100 | alt idx (flips=0, n=3) | X rand (flips=1) | Y rand |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | -2 | -2 |
| 1234567 | 42 | 3 | 5 | 4466845103 | 1649942136 | 36 | 0 | -2 | -2 |
| 9999998 | 1 | 0 | 0 | 9999999 | 402872864 | 64 | 2 | 1 | 1 |
| 5000000 | 77 | 255 | 255 | 265976797357 | 245341663 | 63 | 1 | 2 | 2 |
| -42 | 3 | 10 | 7 | 8425470732 | 595483585 | 85 | 1 | 1 | 1 |
| 31337 | 9001 | 1000 | 2000 | 1711291959338 | 1594335480 | 80 | 0 | 3 | 3 |

(`h0` in the table is `seed + uid + cx*374761393 + cy*668265263`, i.e. the chance-hash input with flips = 0. The alt/X/Y columns recompute `h0` with their own `+flips` / `+1` term.)

## Appendix: `hxd_Perlin.GRADIENTS` (renderer_fixed.js:315563, verbatim)

768 values: 256 gradients × (x, y, z). Index `i` uses `GRADIENTS[3i] * 2.12` and `GRADIENTS[3i+1] * 2.12`.

```
hxd_Perlin.GRADIENTS = [-0.763874,-0.596439,-0.246489,0.396055,0.904518,-0.158073,-0.499004,-0.8665,-0.0131631,0.468724,-0.824756,0.316346,0.829598,0.43195,0.353816,-0.454473,0.629497,-0.630228,-0.162349,-0.869962,-0.465628,0.932805,0.253451,0.256198,-0.345419,0.927299,-0.144227,-0.715026,-0.293698,-0.634413,-0.245997,0.717467,-0.651711,-0.967409,-0.250435,-0.037451,0.901729,0.397108,-0.170852,0.892657,-0.0720622,-0.444938,0.0260084,-0.0361701,0.999007,0.949107,-0.19486,0.247439,0.471803,-0.807064,-0.355036,0.879737,0.141845,0.453809,0.570747,0.696415,0.435033,-0.141751,-0.988233,-0.0574584,-0.58219,-0.0303005,0.812488,-0.60922,0.239482,-0.755975,0.299394,-0.197066,-0.933557,-0.851615,-0.220702,-0.47544,0.848886,0.341829,-0.403169,-0.156129,-0.687241,0.709453,-0.665651,0.626724,0.405124,0.595914,-0.674582,0.43569,0.171025,-0.509292,0.843428,0.78605,0.536414,-0.307222,0.18905,-0.791613,0.581042,-0.294916,0.844994,0.446105,0.342031,-0.58736,-0.7335,0.57155,0.7869,0.232635,0.885026,-0.408223,0.223791,-0.789518,0.571645,0.223347,0.774571,0.31566,0.548087,-0.79695,-0.0433603,-0.602487,-0.142425,-0.473249,-0.869339,-0.0698838,0.170442,0.982886,0.687815,-0.484748,0.540306,0.543703,-0.534446,-0.647112,0.97186,0.184391,-0.146588,0.707084,0.485713,-0.513921,0.942302,0.331945,0.043348,0.499084,0.599922,0.625307,-0.289203,0.211107,0.9337,0.412433,-0.71667,-0.56239,0.87721,-0.082816,0.47291,-0.420685,-0.214278,0.881538,0.752558,-0.0391579,0.657361,0.0765725,-0.996789,0.0234082,-0.544312,-0.309435,-0.779727,-0.455358,-0.415572,0.787368,-0.874586,0.483746,0.0330131,0.245172,-0.0838623,0.965846,0.382293,-0.432813,0.81641,-0.287735,-0.905514,0.311853,-0.667704,0.704955,-0.239186,0.717885,-0.464002,-0.518983,0.976342,-0.214895,0.0240053,-0.0733096,-0.921136,0.382276,-0.986284,0.151224,-0.0661379,-0.899319,-0.429671,0.0812908,0.652102,-0.724625,0.222893,0.203761,0.458023,-0.865272,-0.030396,0.698724,-0.714745,-0.460232,0.839138,0.289887,-0.0898602,0.837894,0.538386,-0.731595,0.0793784,0.677102,-0.447236,-0.788397,0.422386,0.186481,0.645855,-0.740335,-0.259006,0.935463,0.240467,0.445839,0.819655,-0.359712,0.349962,0.755022,-0.554499,-0.997078,-0.0359577,0.0673977,-0.431163,-0.147516,-0.890133,0.299648,-0.63914,0.708316,0.397043,0.566526,-0.722084,-0.502489,0.438308,-0.745246,0.0687235,0.354097,0.93268,-0.0476651,-0.462597,0.885286,-0.221934,0.900739,-0.373383,-0.956107,-0.225676,0.186893,-0.187627,0.391487,-0.900852,-0.224209,-0.315405,0.92209,-0.730807,-0.537068,0.421283,-0.0353135,-0.816748,0.575913,-0.941391,0.176991,-0.287153,-0.154174,0.390458,0.90762,-0.283847,0.533842,0.796519,-0.482737,-0.850448,0.209052,-0.649175,0.477748,0.591886,0.885373,-0.405387,-0.227543,-0.147261,0.181623,-0.972279,0.0959236,-0.115847,-0.988624,-0.89724,-0.191348,0.397928,0.903553,-0.428461,-0.00350461,0.849072,-0.295807,-0.437693,0.65551,0.741754,-0.141804,0.61598,-0.178669,0.767232,0.0112967,0.932256,-0.361623,-0.793031,0.258012,0.551845,0.421933,0.454311,0.784585,-0.319993,0.0401618,-0.946568,-0.81571,0.551307,-0.175151,-0.377644,0.00322313,0.925945,0.129759,-0.666581,-0.734052,0.601901,-0.654237,-0.457919,-0.927463,-0.0343576,-0.372334,-0.438663,-0.868301,-0.231578,-0.648845,-0.749138,-0.133387,0.507393,-0.588294,0.629653,0.726958,0.623665,0.287358,0.411159,0.367614,-0.834151,0.806333,0.585117,-0.0864016,0.263935,-0.880876,0.392932,0.421546,-0.201336,0.884174,-0.683198,-0.569557,-0.456996,-0.117116,-0.0406654,-0.992285,-0.643679,-0.109196,-0.757465,-0.561559,-0.62989,0.536554,0.0628422,0.104677,-0.992519,0.480759,-0.2867,-0.828658,-0.228559,-0.228965,-0.946222,-0.10194,-0.65706,-0.746914,0.0689193,-0.678236,0.731605,0.401019,-0.754026,0.52022,-0.742141,0.547083,-0.387203,-0.00210603,-0.796417,-0.604745,0.296725,-0.409909,-0.862513,-0.260932,-0.798201,0.542945,-0.641628,0.742379,0.192838,-0.186009,-0.101514,0.97729,0.106711,-0.962067,0.251079,-0.743499,0.30988,-0.592607,-0.795853,-0.605066,-0.0226607,-0.828661,-0.419471,-0.370628,0.0847218,-0.489815,-0.8677,-0.381405,0.788019,-0.483276,0.282042,-0.953394,0.107205,0.530774,0.847413,0.0130696,0.0515397,0.922524,0.382484,-0.631467,-0.709046,0.313852,0.688248,0.517273,0.508668,0.646689,-0.333782,-0.685845,-0.932528,-0.247532,-0.262906,0.630609,0.68757,-0.359973,0.577805,-0.394189,0.714673,-0.887833,-0.437301,-0.14325,0.690982,0.174003,0.701617,-0.866701,0.0118182,0.498689,-0.482876,0.727143,0.487949,-0.577567,0.682593,-0.447752,0.373768,0.0982991,0.922299,0.170744,0.964243,-0.202687,0.993654,-0.035791,-0.106632,0.587065,0.4143,-0.695493,-0.396509,0.26509,-0.878924,-0.0866853,0.83553,-0.542563,0.923193,0.133398,-0.360443,0.00379108,-0.258618,0.965972,0.239144,0.245154,-0.939526,0.758731,-0.555871,0.33961,0.295355,0.309513,0.903862,0.0531222,-0.91003,-0.411124,0.270452,0.0229439,-0.96246,0.563634,0.0324352,0.825387,0.156326,0.147392,0.976646,-0.0410141,0.981824,0.185309,-0.385562,-0.576343,-0.720535,0.388281,0.904441,0.176702,0.945561,-0.192859,-0.262146,0.844504,0.520193,0.127325,0.0330893,0.999121,-0.0257505,-0.592616,-0.482475,-0.644999,0.539471,0.631024,-0.557476,0.655851,-0.027319,-0.754396,0.274465,0.887659,0.369772,-0.123419,0.975177,-0.183842,-0.223429,0.708045,0.66989,-0.908654,0.196302,0.368528,-0.95759,-0.00863708,0.288005,0.960535,0.030592,0.276472,-0.413146,0.907537,0.0754161,-0.847992,0.350849,-0.397259,0.614736,0.395841,0.68221,-0.503504,-0.666128,-0.550234,-0.268833,-0.738524,-0.618314,0.792737,-0.60001,-0.107502,-0.637582,0.508144,-0.579032,0.750105,0.282165,-0.598101,-0.351199,-0.392294,-0.850155,0.250126,-0.960993,-0.118025,-0.732341,0.680909,-0.0063274,-0.760674,-0.141009,0.633634,0.222823,-0.304012,0.926243,0.209178,0.505671,0.836984,0.757914,-0.56629,-0.323857,-0.782926,-0.339196,0.52151,-0.462952,0.585565,0.665424,0.61879,0.194119,-0.761194,0.741388,-0.276743,0.611357,0.707571,0.702621,0.0752872,0.156562,0.819977,0.550569,-0.793606,0.440216,0.42,0.234547,0.885309,-0.401517,0.132598,0.80115,-0.58359,-0.377899,-0.639179,0.669808,-0.865993,-0.396465,0.304748,-0.624815,-0.44283,0.643046,-0.485705,0.825614,-0.287146,-0.971788,0.175535,0.157529,-0.456027,0.392629,0.798675,-0.0104443,0.521623,-0.853112,-0.660575,-0.74519,0.091282,-0.0157698,-0.307475,-0.951425,-0.603467,-0.250192,0.757121,0.506876,0.25006,0.824952,0.255404,0.966794,0.00884498,0.466764,-0.874228,-0.133625,0.475077,-0.0682351,-0.877295,-0.224967,-0.938972,-0.260233,-0.377929,-0.814757,-0.439705,-0.305847,0.542333,-0.782517,0.26658,-0.902905,-0.337191,0.0275773,0.322158,-0.946284,0.0185422,0.716349,0.697496,-0.20483,0.978416,0.0273371,-0.898276,0.373969,0.230752,-0.00909378,0.546594,0.837349,0.6602,-0.751089,0.000959236,0.855301,-0.303056,0.420259,0.797138,0.0623013,-0.600574,0.48947,-0.866813,0.0951509,0.251142,0.674531,0.694216,-0.578422,-0.737373,-0.348867,-0.254689,-0.514807,0.818601,0.374972,0.761612,0.528529,0.640303,-0.734271,-0.225517,-0.638076,0.285527,0.715075,0.772956,-0.15984,-0.613995,0.798217,-0.590628,0.118356,-0.986276,-0.0578337,-0.154644,-0.312988,-0.94549,0.0899272,-0.497338,0.178325,0.849032,-0.101136,-0.981014,0.165477,-0.521688,0.0553434,-0.851339,-0.786182,-0.583814,0.202678,-0.565191,0.821858,-0.0714658,0.437895,0.152598,-0.885981,-0.92394,0.353436,-0.14635,0.212189,-0.815162,-0.538969,-0.859262,0.143405,-0.491024,0.991353,0.112814,0.0670273,0.0337884,-0.979891,-0.196654];
```
