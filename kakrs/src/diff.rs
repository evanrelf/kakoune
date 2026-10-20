//! Port of diff.hh
//!
//! Implementation of the linear space variant of the algorithm described in
//! "An O(ND) Difference Algorithm and Its Variations"
//! (http://xmailserver.org/diff2.pdf)
//!
//! Sequences are abstracted through an equality predicate on indices.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SnakeOp {
    #[default]
    Add,
    Del,
    RevAdd,
    RevDel,
}

/// A snake is an edit followed by a (possibly empty) diagonal
#[derive(Clone, Copy, Debug, Default)]
struct Snake {
    // The end points of the diagonal (x, y) -> (u, v)
    x: i32,
    y: i32,
    u: i32,
    v: i32,
    op: SnakeOp,
}

fn rev_op(op: SnakeOp) -> SnakeOp {
    match op {
        SnakeOp::Add => SnakeOp::RevAdd,
        SnakeOp::Del => SnakeOp::RevDel,
        o => o,
    }
}

/// V arrays indexed by diagonal k, which can be negative
struct VArray {
    data: Vec<i32>,
    offset: i32,
}

impl VArray {
    fn get(&self, k: i32) -> i32 {
        self.data[(k + self.offset) as usize]
    }
    fn set(&mut self, k: i32, v: i32) {
        self.data[(k + self.offset) as usize] = v;
    }
}

/// eq(i, j) compares a[a_off + i] with b[b_off + j] (indices relative to the subrange)
fn find_end_snake(
    forward: bool,
    n: i32,
    m: i32,
    v_arr: &VArray,
    d: i32,
    k: i32,
    eq: &dyn Fn(i32, i32) -> bool,
) -> Snake {
    let add = k == -d || (k != d && v_arr.get(k - 1) < v_arr.get(k + 1));
    // if diagonal on the right goes further along x than diagonal on the left,
    // then we take a vertical edge from it to this diagonal, hence x = V[k+1]
    // else, we take an horizontal edge from our left diagonal,x = V[k-1]+1
    let x = if add { v_arr.get(k + 1) } else { v_arr.get(k - 1) + 1 };
    // we are by construction on diagonal k, so our position along b (y) is x - k.
    let y = x - k;
    let mut u = x;
    let mut v = y;
    // follow end snake along diagonal k
    while u < n && v < m && (if forward { eq(u, v) } else { eq(n - 1 - u, m - 1 - v) }) {
        u += 1;
        v += 1;
    }
    Snake { x, y, u, v, op: if add { SnakeOp::Add } else { SnakeOp::Del } }
}

fn find_middle_snake(
    n: i32,
    m: i32,
    v1: &mut VArray,
    v2: &mut VArray,
    cost_limit: i32,
    eq: &dyn Fn(i32, i32) -> bool,
) -> Snake {
    let delta = n - m;
    v1.set(1, 0);
    v2.set(1, 0);

    let max_d = ((m + n + 1) / 2 + 1).min(cost_limit);
    for d in 0..max_d {
        let mut k1 = -d;
        while k1 <= d {
            let p = find_end_snake(true, n, m, v1, d, k1, eq);
            v1.set(k1, p.u);
            let k2 = -(k1 - delta);
            if delta % 2 != 0 && -(d - 1) <= k2 && k2 <= (d - 1) && v1.get(k1) + v2.get(k2) >= n {
                return p; // return last snake on forward path, len = (2 * D - 1)
            }
            k1 += 2;
        }

        let mut k2 = -d;
        while k2 <= d {
            let p = find_end_snake(false, n, m, v2, d, k2, eq);
            v2.set(k2, p.u);
            let k1 = -(k2 - delta);
            if delta % 2 == 0 && -d <= k1 && k1 <= d && v1.get(k1) + v2.get(k2) >= n {
                // return last snake on reverse path, len = 2 * D
                return Snake { x: n - p.u, y: m - p.v, u: n - p.x, v: m - p.y, op: rev_op(p.op) };
            }
            k2 += 2;
        }
    }

    // We did not find a minimal path in less than max_D iterations, iterate one more time finding the best
    let mut best = Snake::default();
    let score = |s: &Snake| s.u + s.v;
    let mut k1 = -max_d;
    while k1 <= max_d {
        let p = find_end_snake(true, n, m, v1, max_d, k1, eq);
        v1.set(k1, p.u);
        if delta % 2 != 0 && p.u <= n && p.v <= m && score(&p) >= score(&best) {
            best = p;
        }
        k1 += 2;
    }
    let mut k2 = -max_d;
    while k2 <= max_d {
        let p = find_end_snake(false, n, m, v2, max_d, k2, eq);
        v2.set(k2, p.u);
        if delta % 2 == 0 && p.u <= n && p.v <= m && score(&p) >= score(&best) {
            best = Snake { x: p.x, y: p.y, u: p.u, v: p.v, op: rev_op(p.op) };
        }
        k2 += 2;
    }

    if best.op == SnakeOp::RevAdd || best.op == SnakeOp::RevDel {
        // reverse the snake now, as we were comparing snake length
        best = Snake { x: n - best.u, y: m - best.v, u: n - best.x, v: m - best.y, op: best.op };
    }
    best
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DiffOp {
    #[default]
    Keep,
    Add,
    Remove,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Diff {
    pub op: DiffOp,
    pub len: i32,
}

#[allow(clippy::too_many_arguments)]
fn find_diff_rec(
    mut beg_a: i32,
    mut end_a: i32,
    mut beg_b: i32,
    mut end_b: i32,
    v1: &mut VArray,
    v2: &mut VArray,
    cost_limit: i32,
    eq: &dyn Fn(i32, i32) -> bool,
    on_diff: &mut dyn FnMut(DiffOp, i32),
) {
    let on_diff_ifn = |op: DiffOp, len: i32, on_diff: &mut dyn FnMut(DiffOp, i32)| {
        if len != 0 {
            on_diff(op, len);
        }
    };

    let mut prefix_len = 0;
    while beg_a != end_a && beg_b != end_b && eq(beg_a, beg_b) {
        beg_a += 1;
        beg_b += 1;
        prefix_len += 1;
    }

    let mut suffix_len = 0;
    while beg_a != end_a && beg_b != end_b && eq(end_a - 1, end_b - 1) {
        end_a -= 1;
        end_b -= 1;
        suffix_len += 1;
    }

    on_diff_ifn(DiffOp::Keep, prefix_len, on_diff);

    let len_a = end_a - beg_a;
    let len_b = end_b - beg_b;
    if len_a == 0 {
        on_diff_ifn(DiffOp::Add, len_b, on_diff);
    } else if len_b == 0 {
        on_diff_ifn(DiffOp::Remove, len_a, on_diff);
    } else {
        let sub_eq = |i: i32, j: i32| eq(beg_a + i, beg_b + j);
        let snake = find_middle_snake(len_a, len_b, v1, v2, cost_limit, &sub_eq);
        debug_assert!(snake.u <= len_a && snake.v <= len_b);

        find_diff_rec(
            beg_a,
            beg_a + snake.x - (snake.op == SnakeOp::Del) as i32,
            beg_b,
            beg_b + snake.y - (snake.op == SnakeOp::Add) as i32,
            v1,
            v2,
            cost_limit,
            eq,
            on_diff,
        );

        if snake.op == SnakeOp::Add {
            on_diff_ifn(DiffOp::Add, 1, on_diff);
        }
        if snake.op == SnakeOp::Del {
            on_diff_ifn(DiffOp::Remove, 1, on_diff);
        }

        on_diff_ifn(DiffOp::Keep, snake.u - snake.x, on_diff);

        if snake.op == SnakeOp::RevAdd {
            on_diff_ifn(DiffOp::Add, 1, on_diff);
        }
        if snake.op == SnakeOp::RevDel {
            on_diff_ifn(DiffOp::Remove, 1, on_diff);
        }

        find_diff_rec(
            beg_a + snake.u + (snake.op == SnakeOp::RevDel) as i32,
            end_a,
            beg_b + snake.v + (snake.op == SnakeOp::RevAdd) as i32,
            end_b,
            v1,
            v2,
            cost_limit,
            eq,
            on_diff,
        );
    }

    on_diff_ifn(DiffOp::Keep, suffix_len, on_diff);
}

/// Compute the diff between sequences a (len n) and b (len m), eq(i, j)
/// compares a[i] and b[j]. on_diff is called with merged consecutive ops.
pub fn for_each_diff(n: i32, m: i32, eq: impl Fn(i32, i32) -> bool, mut on_diff: impl FnMut(DiffOp, i32)) {
    let max = 2 * (n + m) + 1;
    let mut v1 = VArray { data: vec![0; max as usize], offset: n + m };
    let mut v2 = VArray { data: vec![0; max as usize], offset: n + m };
    const COST_LIMIT: i32 = 1000;
    let mut last = Diff { op: DiffOp::Keep, len: 0 };
    find_diff_rec(0, n, 0, m, &mut v1, &mut v2, COST_LIMIT, &eq, &mut |op, len| {
        if last.op == op {
            last.len += len;
        } else {
            if last.len != 0 {
                on_diff(last.op, last.len);
            }
            last = Diff { op, len };
        }
    });
    if last.op != DiffOp::Keep || last.len != 0 {
        on_diff(last.op, last.len);
    }
}

pub fn compute_diff<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Diff> {
    let mut res = Vec::new();
    for_each_diff(a.len() as i32, b.len() as i32, |i, j| a[i as usize] == b[j as usize], |op, len| {
        res.push(Diff { op, len })
    });
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut res = Vec::new();
        let (mut ia, mut ib) = (0usize, 0usize);
        for d in compute_diff(a, b) {
            let len = d.len as usize;
            match d.op {
                DiffOp::Keep => {
                    res.extend_from_slice(&a[ia..ia + len]);
                    ia += len;
                    ib += len;
                }
                DiffOp::Add => {
                    res.extend_from_slice(&b[ib..ib + len]);
                    ib += len;
                }
                DiffOp::Remove => ia += len,
            }
        }
        res
    }

    #[test]
    fn test_diff() {
        for (a, b) in [("abcabba", "cbabac"), ("", "abc"), ("abc", ""), ("kitten", "sitting"), ("same", "same")] {
            assert_eq!(apply(a.as_bytes(), b.as_bytes()), b.as_bytes());
        }
    }
}
