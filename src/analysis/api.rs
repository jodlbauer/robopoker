use super::response::Sample;
use crate::cards::isomorphism::Isomorphism;
use crate::cards::observation::Observation;
use crate::cards::street::Street;
use crate::clustering::abstraction::Abstraction;
use crate::clustering::histogram::Histogram;
use crate::clustering::metric::Metric;
use crate::clustering::pair::Pair;
use crate::clustering::sinkhorn::Sinkhorn;
use crate::gameplay::path::Path;
use crate::transport::coupling::Coupling;
use crate::Energy;
use crate::Probability;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio_postgres::Client;
use tokio_postgres::Error as E;

pub struct API(Arc<Client>);

impl From<Arc<Client>> for API {
    fn from(client: Arc<Client>) -> Self {
        Self(client)
    }
}

impl API {
    pub async fn new() -> Self {
        Self(crate::db().await)
    }

    // global lookups
    pub async fn obs_to_abs(&self, obs: Observation) -> Result<Abstraction, E> {
        let iso = i64::from(Isomorphism::from(obs));
        const SQL: &'static str = r#"
            SELECT abs
            FROM isomorphism
            WHERE obs = $1
        "#;
        Ok(self
            .0
            .query_one(SQL, &[&iso])
            .await?
            .get::<_, i64>(0)
            .into())
    }
    pub async fn metric(&self, street: Street) -> Result<Metric, E> {
        let street = street as i16;
        const SQL: &'static str = r#"
            SELECT
                a1.abs # a2.abs AS xor,
                m.dx            AS dx
            FROM abstraction a1
            JOIN abstraction a2
                ON a1.street = a2.street
            JOIN metric m
                ON (a1.abs # a2.abs) = m.xor
            WHERE
                a1.street   = $1 AND
                a1.abs     != a2.abs;
        "#;
        Ok(self
            .0
            .query(SQL, &[&street])
            .await?
            .iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Energy>(1)))
            .map(|(xor, distance)| (Pair::from(xor), distance))
            .collect::<BTreeMap<Pair, Energy>>()
            .into())
    }
    pub async fn basis(&self, street: Street) -> Result<Vec<Abstraction>, E> {
        let street = street as i16;
        const SQL: &'static str = r#"
            SELECT a2.abs
            FROM abstraction a2
            JOIN abstraction a1 ON a2.street = a1.street
            WHERE a1.abs = $1;
        "#;
        Ok(self
            .0
            .query(SQL, &[&street])
            .await?
            .iter()
            .map(|row| row.get::<_, i64>(0))
            .map(Abstraction::from)
            .collect())
    }

    // equity calculations
    pub async fn abs_equity(&self, abs: Abstraction) -> Result<Probability, E> {
        let iso = i64::from(abs);
        const SQL: &'static str = r#"
            SELECT equity
            FROM abstraction
            WHERE abs = $1
        "#;
        Ok(self
            .0
            .query_one(SQL, &[&iso])
            .await?
            .get::<_, f32>(0)
            .into())
    }
    pub async fn obs_equity(&self, obs: Observation) -> Result<Probability, E> {
        let iso = i64::from(Isomorphism::from(obs));
        let sql = if obs.street() == Street::Rive {
            r#"
                SELECT equity
                FROM isomorphism
                WHERE obs = $1
            "#
        } else {
            r#"
                SELECT SUM(t.dx * a.equity)
                FROM transitions t
                JOIN isomorphism     e ON e.abs = t.prev
                JOIN abstraction a ON a.abs = t.next
                WHERE e.obs = $1
            "#
        };
        Ok(self
            .0
            .query_one(sql, &[&iso])
            .await?
            .get::<_, f32>(0)
            .into())
    }

    // distance calculations
    pub async fn abs_distance(&self, abs1: Abstraction, abs2: Abstraction) -> Result<Energy, E> {
        if abs1.street() != abs2.street() {
            return Err(E::__private_api_timeout());
        }
        if abs1 == abs2 {
            return Ok(0 as Energy);
        }
        let xor = i64::from(Pair::from((&abs1, &abs2)));
        const SQL: &'static str = r#"
            SELECT m.dx
            FROM metric m
            WHERE $1 = m.xor;
        "#;
        Ok(self.0.query_one(SQL, &[&xor]).await?.get::<_, Energy>(0))
    }
    pub async fn obs_distance(&self, obs1: Observation, obs2: Observation) -> Result<Energy, E> {
        if obs1.street() != obs2.street() {
            return Err(E::__private_api_timeout());
        }
        let (ref hx, ref hy, ref metric) = tokio::try_join!(
            self.obs_histogram(obs1),
            self.obs_histogram(obs2),
            self.metric(obs1.street().next())
        )?;
        Ok(Sinkhorn::from((hx, hy, metric)).minimize().cost())
    }

    // population lookups
    pub async fn abs_population(&self, abs: Abstraction) -> Result<usize, E> {
        let abs = i64::from(abs);
        const SQL: &'static str = r#"
            SELECT population
            FROM abstraction
            WHERE abs = $1
        "#;
        Ok(self.0.query_one(SQL, &[&abs]).await?.get::<_, i32>(0) as usize)
    }
    pub async fn obs_population(&self, obs: Observation) -> Result<usize, E> {
        let iso = i64::from(Isomorphism::from(obs));
        const SQL: &'static str = r#"
            SELECT population
            FROM abstraction
            JOIN isomorphism ON isomorphism.abs = abstraction.abs
            WHERE obs = $1
        "#;
        Ok(self.0.query_one(SQL, &[&iso]).await?.get::<_, i64>(0) as usize)
    }

    // centrality (mean distance) lookups
    pub async fn abs_centrality(&self, abs: Abstraction) -> Result<Probability, E> {
        let abs = i64::from(abs);
        const SQL: &'static str = r#"
            SELECT centrality
            FROM abstraction
            WHERE abs = $1
        "#;
        Ok(self
            .0
            .query_one(SQL, &[&abs])
            .await?
            .get::<_, f32>(0)
            .into())
    }
    pub async fn obs_centrality(&self, obs: Observation) -> Result<Probability, E> {
        let iso = i64::from(Isomorphism::from(obs));
        const SQL: &'static str = r#"
            SELECT centrality
            FROM abstraction
            JOIN isomorphism ON isomorphism.abs = abstraction.abs
            WHERE obs = $1
        "#;
        Ok(self
            .0
            .query_one(SQL, &[&iso])
            .await?
            .get::<_, f32>(0)
            .into())
    }

    // histogram aggregation via join
    pub async fn abs_histogram(&self, abs: Abstraction) -> Result<Histogram, E> {
        let idx = i64::from(abs);
        let mass = abs.street().n_children() as f32;
        const SQL: &'static str = r#"
            SELECT next, dx
            FROM transitions
            WHERE prev = $1
        "#;
        Ok(self
            .0
            .query(SQL, &[&idx])
            .await?
            .iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Energy>(1)))
            .map(|(next, dx)| (next, (dx * mass).round() as usize))
            .map(|(next, dx)| (Abstraction::from(next), dx))
            .fold(Histogram::default(), |mut h, (next, dx)| {
                h.set(next, dx);
                h
            }))
    }
    pub async fn obs_histogram(&self, obs: Observation) -> Result<Histogram, E> {
        // Kd8s~6dJsAc
        let idx = i64::from(Isomorphism::from(obs));
        let mass = obs.street().n_children() as f32;
        const SQL: &'static str = r#"
            SELECT next, dx
            FROM transitions
            JOIN isomorphism ON isomorphism.abs = transitions.prev
            WHERE isomorphism.obs = $1
        "#;
        Ok(self
            .0
            .query(SQL, &[&idx])
            .await?
            .iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Energy>(1)))
            .map(|(next, dx)| (next, (dx * mass).round() as usize))
            .map(|(next, dx)| (Abstraction::from(next), dx))
            .fold(Histogram::default(), |mut h, (next, dx)| {
                h.set(next, dx);
                h
            }))
    }

    // observation similarity lookups
    pub async fn obs_similar(&self, obs: Observation) -> Result<Vec<Observation>, E> {
        let iso = i64::from(Isomorphism::from(obs));
        const SQL: &'static str = r#"
            WITH target AS (
                SELECT abs, population
                FROM isomorphism e
                JOIN abstraction a ON e.abs = a.abs
                WHERE obs = $1
            )
            SELECT e.obs
            FROM isomorphism e
            JOIN target t ON e.abs = t.abs
            WHERE e.obs != $1
                AND e.position < LEAST(5, t.population)  -- Sample from available positions
                AND e.position >= FLOOR(RANDOM() * GREATEST(t.population - 5, 1))  -- Random starting point
            LIMIT 5;
        "#;
        Ok(self
            .0
            .query(SQL, &[&iso])
            .await?
            .iter()
            .map(|row| row.get::<_, i64>(0))
            .map(Observation::from)
            .collect())
    }
    pub async fn abs_similar(&self, abs: Abstraction) -> Result<Vec<Observation>, E> {
        let abs = i64::from(abs);
        const SQL: &'static str = r#"
            WITH target AS (
                SELECT population FROM abstraction WHERE abs = $1
            )
            SELECT obs
            FROM isomorphism e, target t
            WHERE abs = $1
                AND position < LEAST(5, t.population)  -- Sample from available positions
                AND position >= FLOOR(RANDOM() * GREATEST(t.population - 5, 1))  -- Random starting point
            LIMIT 5;
        "#;
        Ok(self
            .0
            .query(SQL, &[&abs])
            .await?
            .iter()
            .map(|row| row.get::<_, i64>(0))
            .map(Observation::from)
            .collect())
    }
    pub async fn replace_obs(&self, obs: Observation) -> Result<Observation, E> {
        const SQL: &'static str = r#"
            -- OBS SWAP
            WITH sample AS (
                SELECT
                    e.abs,
                    a.population,
                    FLOOR(RANDOM() * a.population)::INTEGER as i
                FROM isomorphism    e
                JOIN abstraction    a ON e.abs = a.abs
                WHERE               e.obs = $1
            )
            SELECT          e.obs
            FROM sample     t
            JOIN isomorphism e ON e.abs = t.abs
            AND             e.position = t.i
            LIMIT 1;
        "#;
        //
        let iso = i64::from(Isomorphism::from(obs));
        //
        let row = self.0.query_one(SQL, &[&iso]).await?;
        Ok(Observation::from(row.get::<_, i64>(0)))
    }

    // proximity lookups
    pub async fn abs_nearby(&self, abs: Abstraction) -> Result<Vec<(Abstraction, Energy)>, E> {
        let abs = i64::from(abs);
        const SQL: &'static str = r#"
            SELECT a1.abs, m.dx
            FROM abstraction    a1
            JOIN abstraction    a2 ON a1.street = a2.street
            JOIN metric         m  ON (a1.abs # $1) = m.xor
            WHERE
                a2.abs  = $1 AND
                a1.abs != $1
            ORDER BY m.dx ASC
            LIMIT 5;
        "#;
        Ok(self
            .0
            .query(SQL, &[&abs])
            .await?
            .iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Energy>(1)))
            .map(|(abs, distance)| (Abstraction::from(abs), distance))
            .collect())
    }
    pub async fn obs_nearby(&self, obs: Observation) -> Result<Vec<(Abstraction, Energy)>, E> {
        let iso = i64::from(Isomorphism::from(obs));
        const SQL: &'static str = r#"
            -- OBS NEARBY
            SELECT a.abs, m.dx
            FROM isomorphism        e
            JOIN abstraction    a ON e.abs = a.abs
            JOIN metric         m  ON (a.abs # e.abs) = m.xor
            WHERE
                e.obs   = $1 AND
                a.abs != e.abs
            ORDER BY m.dx ASC
            LIMIT 5;
        "#;
        Ok(self
            .0
            .query(SQL, &[&iso])
            .await?
            .iter()
            .map(|row| (row.get::<_, i64>(0), row.get::<_, Energy>(1)))
            .map(|(abs, distance)| (Abstraction::from(abs), distance))
            .collect())
    }
}

// exploration panel
impl API {
    pub async fn exp_wrt_str(&self, str: Street) -> Result<Sample, E> {
        self.exp_wrt_obs(Observation::from(str)).await
    }
    pub async fn exp_wrt_obs(&self, obs: Observation) -> Result<Sample, E> {
        const SQL: &'static str = r#"
            -- EXP WRT OBS
            SELECT
                e.obs,
                a.abs,
                a.equity::REAL          as equity,
                a.population::REAL / $2 as density,
                a.centrality::REAL      as centrality
            FROM isomorphism e
            JOIN abstraction a ON e.abs = a.abs
            WHERE e.obs = $1;
        "#;
        //
        let n = obs.street().n_observations() as f32;
        let iso = i64::from(Isomorphism::from(obs));
        //
        let row = self.0.query_one(SQL, &[&iso, &n]).await?;
        Ok(Sample::from(row))
    }
    pub async fn exp_wrt_abs(&self, abs: Abstraction) -> Result<Sample, E> {
        const SQL: &'static str = r#"
            -- EXP WRT ABS
            WITH sample AS (
                SELECT
                    a.abs,
                    a.population,
                    a.equity,
                    a.centrality,
                    FLOOR(RANDOM() * a.population)::INTEGER as i
                FROM abstraction a
                WHERE a.abs = $1
            )
            SELECT
                e.obs,
                s.abs,
                s.equity::REAL          as equity,
                s.population::REAL / $2 as density,
                s.centrality::REAL      as centrality
            FROM sample     s
            JOIN isomorphism    e ON e.abs = s.abs
            AND             e.position = s.i
            LIMIT 1;
        "#;
        //
        let n = abs.street().n_isomorphisms() as f32;
        let abs = i64::from(abs);
        //
        let row = self.0.query_one(SQL, &[&abs, &n]).await?;
        Ok(Sample::from(row))
    }
}

// neighborhood lookups
impl API {
    pub async fn nbr_any_wrt_abs(&self, wrt: Abstraction) -> Result<Sample, E> {
        // uniform over abstraction space
        use rand::seq::SliceRandom;
        let ref mut rng = rand::thread_rng();
        let abs = Abstraction::all(wrt.street())
            .into_iter()
            .filter(|&x| x != wrt)
            .collect::<Vec<_>>()
            .choose(rng)
            .copied()
            .unwrap();
        self.nbr_abs_wrt_abs(wrt, abs).await
    }
    pub async fn nbr_abs_wrt_abs(&self, wrt: Abstraction, abs: Abstraction) -> Result<Sample, E> {
        const SQL: &'static str = r#"
            -- NBR ABS WRT ABS
            WITH sample AS (
                SELECT
                    r.abs                                   as abs,
                    r.population                            as population,
                    r.equity                                as equity,
                    FLOOR(RANDOM() * r.population)::INTEGER as i,
                    COALESCE(m.dx, 0)                       as distance
                FROM abstraction    r
                LEFT JOIN metric    m ON m.xor = ($1::BIGINT # $3::BIGINT)
                WHERE               r.abs = $1
            ),
            random_isomorphism AS (
                SELECT e.obs, e.abs, s.equity, s.population, s.distance
                FROM sample s
                JOIN isomorphism e ON e.abs = s.abs AND e.position = s.i
                WHERE e.abs = $1
                LIMIT 1
            )
            SELECT
                obs,
                abs,
                equity::REAL                      as equity,
                population::REAL / $2             as density,
                distance::REAL                    as distance
            FROM random_isomorphism;
        "#;
        //
        let n = wrt.street().n_isomorphisms() as f32;
        let abs = i64::from(abs);
        let wrt = i64::from(wrt);
        //
        let row = self.0.query_one(SQL, &[&abs, &n, &wrt]).await?;
        Ok(Sample::from(row))
    }
    pub async fn nbr_obs_wrt_abs(&self, wrt: Abstraction, obs: Observation) -> Result<Sample, E> {
        const SQL: &'static str = r#"
            -- NBR OBS WRT ABS
            WITH given AS (
                SELECT
                    (obs),
                    (abs),
                    (abs # $3) as xor
                FROM    isomorphism
                WHERE   obs = $1
            )
            SELECT
                g.obs,
                g.abs,
                a.equity::REAL                      as equity,
                a.population::REAL / $2             as density,
                COALESCE(m.dx, 0)::REAL             as distance
            FROM given          g
            JOIN metric         m ON m.xor = g.xor
            JOIN abstraction    a ON a.abs = g.abs
            LIMIT 1;
        "#;
        //
        let n = wrt.street().n_isomorphisms() as f32;
        let iso = i64::from(Isomorphism::from(obs));
        let wrt = i64::from(wrt);
        //
        let row = self.0.query_one(SQL, &[&iso, &n, &wrt]).await?;
        Ok(Sample::from(row))
    }

    pub async fn kfn_wrt_abs(&self, wrt: Abstraction) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
                -- KNN WRT ABS
                WITH nearest AS (
                    SELECT
                        a.abs                                       as abs,
                        a.population                                as population,
                        m.dx                                        as distance,
                        FLOOR(RANDOM() * population)::INTEGER       as sample
                    FROM abstraction    a
                    JOIN metric         m ON m.xor = (a.abs # $1)
                    WHERE               a.street = $2
                    AND                 a.abs   != $1
                    ORDER BY            m.dx DESC
                    LIMIT 5
                )
                SELECT
                    e.obs,
                    n.abs,
                    a.equity::REAL          as equity,
                    a.population::REAL / $3 as density,
                    n.distance::REAL        as distance
                FROM nearest n
                JOIN abstraction    a ON a.abs = n.abs
                JOIN isomorphism        e ON e.abs = n.abs
                AND                 e.position = n.sample
                ORDER BY            n.distance DESC;
            "#;
        //
        let n = wrt.street().n_isomorphisms() as f32;
        let s = wrt.street() as i16;
        let wrt = i64::from(wrt);
        //
        let rows = self.0.query(SQL, &[&wrt, &s, &n]).await?;
        Ok(rows.into_iter().map(Sample::from).collect())
    }
    pub async fn knn_wrt_abs(&self, wrt: Abstraction) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
            -- KNN WRT ABS
            WITH nearest AS (
                SELECT
                    a.abs                                       as abs,
                    a.population                                as population,
                    m.dx                                        as distance,
                    FLOOR(RANDOM() * population)::INTEGER       as sample
                FROM abstraction    a
                JOIN metric         m ON m.xor = (a.abs # $1)
                WHERE               a.street = $2
                AND                 a.abs   != $1
                ORDER BY            m.dx ASC
                LIMIT 5
            )
            SELECT
                e.obs,
                n.abs,
                a.equity::REAL          as equity,
                a.population::REAL / $3 as density,
                n.distance::REAL        as distance
            FROM nearest n
            JOIN abstraction    a ON a.abs = n.abs
            JOIN isomorphism        e ON e.abs = n.abs
            AND                 e.position = n.sample
            ORDER BY            n.distance ASC;
        "#;
        //
        let n = wrt.street().n_isomorphisms() as f32;
        let s = wrt.street() as i16;
        let wrt = i64::from(wrt);
        //
        let rows = self.0.query(SQL, &[&wrt, &s, &n]).await?;
        Ok(rows.into_iter().map(Sample::from).collect())
    }
    pub async fn kgn_wrt_abs(
        &self,
        wrt: Abstraction,
        nbr: Vec<Observation>,
    ) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
            -- KGN WRT ABS
            WITH input(obs, ord) AS (
              SELECT unnest($3::BIGINT[])                   AS obs,
                     generate_series(1, array_length($3,1)) AS ord
            )
            SELECT
              e.obs AS obs,
              e.abs AS abs,
              a.equity::REAL AS equity,
              a.population::REAL / $1 AS density,
              m.dx::REAL AS distance
            FROM input i
            JOIN isomorphism     e ON e.obs = i.obs
            JOIN abstraction a ON e.abs = a.abs
            JOIN metric      m ON m.xor = (a.abs # $2)
            ORDER BY i.ord
            LIMIT 5;
        "#;
        let isos = nbr
            .into_iter()
            .map(Isomorphism::from)
            .map(i64::from)
            .collect::<Vec<_>>();
        let n = wrt.street().n_isomorphisms() as f32;
        let wrt = i64::from(wrt);
        //
        let rows = self.0.query(SQL, &[&n, &wrt, &&isos]).await?;
        Ok(rows.into_iter().map(Sample::from).collect())
    }
}

// distribution lookups
impl API {
    pub async fn hst_wrt_obs(&self, obs: Observation) -> Result<Vec<Sample>, E> {
        if obs.street() == Street::Rive {
            self.hst_wrt_obs_on_river(obs).await
        } else {
            self.hst_wrt_obs_on_other(obs).await
        }
    }
    pub async fn hst_wrt_abs(&self, abs: Abstraction) -> Result<Vec<Sample>, E> {
        if abs.street() == Street::Rive {
            self.hst_wrt_abs_on_river(abs).await
        } else {
            self.hst_wrt_abs_on_other(abs).await
        }
    }

    async fn hst_wrt_obs_on_river(&self, obs: Observation) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
            -- RIVER OBS DISTRIBUTION
            WITH sample AS (
                SELECT
                    e.obs,
                    e.abs,
                    a.equity,
                    a.population,
                    a.centrality,
                    FLOOR(RANDOM() * a.population)::INTEGER as position
                FROM isomorphism        e
                JOIN abstraction    a ON e.abs = a.abs
                WHERE               e.abs = (SELECT abs FROM isomorphism WHERE obs = $2)
                LIMIT 5
            )
            SELECT
                s.obs                   as obs,
                s.abs                   as abs,
                s.equity::REAL          as equity,
                s.population::REAL / $1 as density,
                s.centrality::REAL      as distance
            FROM sample s;
        "#;
        let n = Street::Rive.n_isomorphisms() as f32;
        let iso = i64::from(Isomorphism::from(obs));
        let rows = self.0.query(SQL, &[&n, &iso]).await?;
        Ok(rows.into_iter().map(Sample::from).collect())
    }

    async fn hst_wrt_obs_on_other(&self, obs: Observation) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
        -- OTHER OBS DISTRIBUTION
            SELECT
                e.obs, e.abs, a.equity
            FROM isomorphism    e
            JOIN abstraction    a ON e.abs = a.abs
            WHERE               e.obs = ANY($1);
        "#;
        let n = obs.street().n_children();
        let children = obs
            .children()
            .map(Isomorphism::from)
            .map(Observation::from)
            .collect::<Vec<_>>();
        let distinct = children
            .iter()
            .copied()
            .map(i64::from)
            .fold(HashSet::<i64>::new(), |mut set, x| {
                set.insert(x);
                set
            })
            .into_iter()
            .collect::<Vec<_>>();
        let rows = self.0.query(SQL, &[&distinct]).await?;
        let rows = rows
            .into_iter()
            .map(|row| {
                (
                    Observation::from(row.get::<_, i64>(0)),
                    Abstraction::from(row.get::<_, i64>(1)),
                    Probability::from(row.get::<_, f32>(2)),
                )
            })
            .map(|(obs, abs, equity)| (obs, (abs, equity)))
            .collect::<BTreeMap<_, _>>();
        let hist = children
            .iter()
            .map(|child| (child, rows.get(child).expect("lookup in db")))
            .fold(BTreeMap::<_, _>::new(), |mut btree, (obs, (abs, eqy))| {
                btree.entry(abs).or_insert((obs, eqy, 0)).2 += 1;
                btree
            })
            .into_iter()
            .map(|(abs, (obs, eqy, pop))| Sample {
                obs: obs.equivalent(),
                abs: abs.to_string(),
                equity: eqy.clone(),
                density: pop as Probability / n as Probability,
                distance: 0.,
            })
            .collect::<Vec<_>>();
        Ok(hist)
    }

    async fn hst_wrt_abs_on_river(&self, abs: Abstraction) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
            -- RIVER ABS DISTRIBUTION
            WITH sample AS (
                SELECT
                    a.abs,
                    a.population,
                    a.equity,
                    a.centrality,
                    FLOOR(RANDOM() * a.population)::INTEGER as position
                FROM abstraction a
                WHERE a.abs = $2
                LIMIT 5
            )
            SELECT
                e.obs                   as obs,
                e.abs                   as abs,
                s.equity::REAL          as equity,
                s.population::REAL / $1 as density,
                s.centrality::REAL      as distance
            FROM sample         s
            JOIN isomorphism    e ON e.abs = s.abs
            AND                 e.position = s.position;
        "#;
        //
        let ref n = Street::Rive.n_isomorphisms() as f32;
        let ref abs = i64::from(abs);
        //
        let rows = self.0.query(SQL, &[n, abs]).await?;
        Ok(rows.into_iter().map(Sample::from).collect())
    }
    async fn hst_wrt_abs_on_other(&self, abs: Abstraction) -> Result<Vec<Sample>, E> {
        const SQL: &'static str = r#"
            -- OTHER ABS DISTRIBUTION
            WITH histogram AS (
                SELECT
                    p.abs                                   as abs,
                    g.dx                                    as probability,
                    p.population                            as population,
                    p.equity                                as equity,
                    p.centrality                            as centrality,
                    FLOOR(RANDOM() * p.population)::INTEGER as i
                FROM transitions g
                JOIN abstraction p ON p.abs = g.next
                WHERE            g.prev = $1
                LIMIT 64
            )
            SELECT
                e.obs              as obs,
                t.abs              as abs,
                t.equity::REAL     as equity,
                t.probability      as density,
                t.centrality::REAL as distance
            FROM histogram      t
            JOIN isomorphism    e ON e.abs = t.abs
            AND                 e.position = t.i
            ORDER BY            t.probability DESC;
        "#;
        //
        let ref abs = i64::from(abs);
        //
        let rows = self.0.query(SQL, &[abs]).await?;
        Ok(rows.into_iter().map(Sample::from).collect())
    }
}

use crate::analysis::response::Decision;
use crate::gameplay::recall::Recall;

// blueprint lookups
impl API {
    pub async fn policy(&self, recall: Recall) -> Result<Vec<Decision>, E> {
        const SQL: &'static str = r#"
        -- policy is indexed by present, past, future
        -- and it returns a vector of decision probabilities
        -- over the set of "choices" we can continue toward

            SELECT edge, policy
            FROM blueprint
            WHERE past    = $1
            AND   present = $2
            AND   future  = $3
        "#;
        let ref game = recall.head();
        let observation = game.sweat();
        let abstraction = self.obs_to_abs(observation).await?;
        let history = recall.path();
        let present = abstraction;
        let futures = Path::from(crate::mccfr::nlhe::encoder::Encoder::choices(game, 0));
        let ref history = i64::from(history);
        let ref present = i64::from(present);
        let ref futures = i64::from(futures);
        let rows = self.0.query(SQL, &[history, present, futures]).await?;
        Ok(rows.into_iter().map(Decision::from).collect())
    }
}
