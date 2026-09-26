use crate::{
    crypto,
    error::{Error, Result, require},
    gateway::{Checkout, VerifiedEvent},
    now,
    wire::{CreatePaymentRequest, Payment, PaymentEvent},
};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub payment: Payment,
    pub input: CreatePaymentRequest,
    pub merchant_id: String,
    pub idempotency_key: String,
    pub request_hash: String,
    pub gateway_identity: String,
    pub notify_url: String,
    pub session_id: String,
    pub provider_payment_id: String,
}
pub struct Outgoing {
    pub id: String,
    pub merchant_id: String,
    pub url: String,
    pub payload: Vec<u8>,
    pub attempt: u32,
}
#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
    _lock: Option<Arc<File>>,
}
fn decode(s: String) -> Result<Record> {
    serde_json::from_str(&s).map_err(|_| Error::internal())
}
fn record(c: &Connection, id: &str) -> Result<Record> {
    decode(
        c.query_row("SELECT data FROM payments WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(Error::not_found())?,
    )
}
fn save(c: &Connection, r: &Record) -> Result<()> {
    c.execute("UPDATE payments SET data=?1,status=?2,provider_order_id=NULLIF(?3,''),provider_payment_id=NULLIF(?4,'') WHERE id=?5",
        params![serde_json::to_string(r)?,r.payment.status,r.payment.provider_order_id,r.provider_payment_id,r.payment.id])?;
    Ok(())
}
fn view(c: &Connection, mut p: Payment) -> Result<Payment> {
    p.notification_status = c
        .query_row(
            "SELECT status FROM outbox WHERE payment_id=?1",
            [&p.id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "not_scheduled".into());
    Ok(p)
}
impl Database {
    pub fn open(path: &str, cache_kib: u32) -> anyhow::Result<Self> {
        let lock = if path == ":memory:" {
            None
        } else {
            let p = Path::new(path);
            if let Some(parent) = p.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = OpenOptions::new();
            options.create(true).truncate(false).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let f = options.open(format!("{path}.lockfile"))?;
            FileExt::try_lock_exclusive(&f).map_err(|_| {
                anyhow::anyhow!("database already in use; run only one service per SQLite file")
            })?;
            let _database_file = options.open(path)?;
            Some(Arc::new(f))
        };
        let c = Connection::open(path)?;
        c.busy_timeout(Duration::from_secs(2))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA temp_store=FILE; PRAGMA mmap_size=0; PRAGMA journal_size_limit=1048576;")?;
        c.pragma_update(None, "cache_size", -(i64::from(cache_kib)))?;
        let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
        anyhow::ensure!(version <= 1, "database schema is newer than this binary");
        if version == 0 {
            c.execute_batch(include_str!("../migrations/001_init.sql"))?;
        }
        // A crash during checkout creation is indeterminate, never permission to charge again.
        c.execute("UPDATE payments SET status='unknown',data=json_set(data,'$.payment.status','unknown','$.payment.updated_at',?1) WHERE status='creating'",[now()])?;
        Ok(Self {
            connection: Arc::new(Mutex::new(c)),
            _lock: lock,
        })
    }
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = connection.lock().map_err(|_| Error::internal())?;
            f(&mut c)
        })
        .await
        .map_err(|_| Error::internal())?
    }
    pub async fn get(&self, merchant_id: &str, id: &str) -> Result<Payment> {
        let m = merchant_id.to_owned();
        let id = id.to_owned();
        self.call(move |c| {
            let data = c
                .query_row(
                    "SELECT data FROM payments WHERE id=?1 AND merchant_id=?2",
                    params![id, m],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or(Error::not_found())?;
            view(c, decode(data)?.payment)
        })
        .await
    }
    pub async fn existing(
        &self,
        merchant_id: &str,
        key: &str,
        hash: &str,
    ) -> Result<Option<Payment>> {
        let (m, k, h) = (merchant_id.to_owned(), key.to_owned(), hash.to_owned());
        self.call(move |c| {
            let data: Option<String> = c
                .query_row(
                    "SELECT data FROM payments WHERE merchant_id=?1 AND idempotency_key=?2",
                    params![m, k],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(data) = data {
                let r = decode(data)?;
                if r.request_hash != h {
                    return Err(Error::conflict("idempotency_conflict"));
                }
                return Ok(Some(view(c, r.payment)?));
            }
            Ok(None)
        })
        .await
    }
    pub async fn reserve(&self, r: Record) -> Result<(Record, bool)> {
        self.call(move |c| {
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let previous:Option<String> = tx.query_row("SELECT data FROM payments WHERE merchant_id=?1 AND idempotency_key=?2",params![r.merchant_id,r.idempotency_key],|row|row.get(0)).optional()?;
            if let Some(data) = previous {
                let previous = decode(data)?;
                if previous.request_hash != r.request_hash { return Err(Error::conflict("idempotency_conflict")); }
                return Ok((previous,false));
            }
            let duplicate:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM payments WHERE merchant_id=?1 AND merchant_order_id=?2)",params![r.merchant_id,r.input.merchant_order_id],|row|row.get(0))?;
            if duplicate { return Err(Error::conflict("merchant_order_exists")); }
            tx.execute("INSERT INTO payments(id,merchant_id,merchant_order_id,idempotency_key,gateway_id,status,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![r.payment.id,r.merchant_id,r.input.merchant_order_id,r.idempotency_key,r.payment.gateway_id,r.payment.status,serde_json::to_string(&r)?])?;
            tx.commit()?;
            Ok((r,true))
        }).await
    }
    pub async fn finish_create(&self, id: &str, checkout: Option<Checkout>) -> Result<Payment> {
        let id = id.to_owned();
        self.call(move |c| {
            let tx = c.transaction()?;
            let mut r = record(&tx, &id)?;
            match checkout {
                Some(v) => {
                    // A verified callback may have arrived before this API response.
                    require(
                        r.session_id.is_empty() || r.session_id == v.session_id,
                        "checkout_session_mismatch",
                    )?;
                    r.payment.checkout_url = v.url;
                    r.session_id = v.session_id;
                    if r.payment.status == "creating" {
                        r.payment.status = "pending".into();
                    }
                }
                None => {
                    if r.payment.status == "creating" {
                        r.payment.status = "unknown".into();
                    }
                }
            }
            r.payment.updated_at = now();
            save(&tx, &r)?;
            let p = view(&tx, r.payment)?;
            tx.commit()?;
            Ok(p)
        })
        .await
    }
    pub async fn accept(&self, gateway_id: &str, identity: &str, e: VerifiedEvent) -> Result<()> {
        let gid = gateway_id.to_owned();
        let identity = identity.to_owned();
        self.call(move |c| {
            let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let mut r=record(&tx,&e.payment_id)?;
            if r.payment.gateway_id != gid || r.gateway_identity != identity {return Err(Error::conflict("gateway_identity_mismatch"));}
            require(r.payment.currency==e.currency, "currency_mismatch")?;
            require(e.product.as_ref().is_none_or(|p|p==&r.input.product), "product_mismatch")?;
            require(e.request_hash.as_ref().is_none_or(|h|h==&r.request_hash), "checkout_reference_mismatch")?;
            if let Some(checkout_id)=&e.checkout_id {
                require(r.session_id.is_empty() || &r.session_id==checkout_id, "checkout_session_mismatch")?;
                r.session_id=checkout_id.clone();
            }
            require(e.method.as_ref().is_none_or(|m|m==&r.input.method), "method_mismatch")?;
            require(e.total_minor>0 && e.charged_minor==e.total_minor, "charged_total_mismatch")?;
            let expected=match r.payment.amount_basis.as_str() {"total"=>Some(e.total_minor),"subtotal"=>e.subtotal_minor,_=>None};
            require(expected==Some(r.payment.amount_minor), "amount_mismatch")?;
            let previous:Option<(String,String)> = tx.query_row("SELECT fingerprint,payment_id FROM receipts WHERE gateway_id=?1 AND event_id=?2",params![gid,e.id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            if let Some((hash,id))=previous {
                if hash!=e.fingerprint || id!=e.payment_id {return Err(Error::conflict("event_reused_with_different_payload"));}
                return Ok(());
            }
            let reused:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM payments WHERE gateway_id=?1 AND id<>?2 AND (provider_order_id=?3 OR provider_payment_id=?4))",params![gid,e.payment_id,e.provider_order_id,e.provider_payment_id],|row|row.get(0))?;
            if reused {return Err(Error::conflict("provider_payment_already_bound"));}
            if r.payment.status=="succeeded" {
                require(r.payment.provider_order_id==e.provider_order_id && r.provider_payment_id==e.provider_payment_id && r.payment.charged_minor==e.charged_minor,"conflicting_payment_result")?;
            } else {
                r.payment.status="succeeded".into(); r.payment.updated_at=now(); r.payment.charged_minor=e.charged_minor;
                r.payment.provider_order_id=e.provider_order_id; r.provider_payment_id=e.provider_payment_id;
                r.payment.notification_status="pending".into(); save(&tx,&r)?;
                let id=crypto::random_id("evt_")?;
                let mut payment=r.payment.clone(); payment.checkout_url.clear();
                let event=PaymentEvent{id:id.clone(),event_type:"payment.succeeded".into(),occurred_at:now(),payment:Some(payment)};
                tx.execute("INSERT INTO outbox(id,merchant_id,payment_id,url,payload,status,due) VALUES(?1,?2,?3,?4,?5,'pending',?6)",params![id,r.merchant_id,r.payment.id,r.notify_url,serde_json::to_vec(&event)?,now()])?;
            }
            tx.execute("INSERT INTO receipts(gateway_id,event_id,fingerprint,payment_id,received_at) VALUES(?1,?2,?3,?4,?5)",params![gid,e.id,e.fingerprint,e.payment_id,now()])?;
            tx.commit()?; Ok(())
        }).await
    }
    pub async fn claim(&self) -> Result<Option<Outgoing>> {
        self.call(move |c| {
            let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute("UPDATE outbox SET status='dead' WHERE status='delivering' AND lease_until<=?1 AND attempts>=16",[now()])?;
            let job=tx.query_row("SELECT id,merchant_id,url,payload,attempts FROM outbox WHERE attempts<16 AND ((status='pending' AND due<=?1) OR (status='delivering' AND lease_until<=?1)) ORDER BY due LIMIT 1",[now()],|r|Ok(Outgoing{id:r.get(0)?,merchant_id:r.get(1)?,url:r.get(2)?,payload:r.get(3)?,attempt:r.get::<_,u32>(4)?+1})).optional()?;
            if let Some(job)=&job {tx.execute("UPDATE outbox SET status='delivering',attempts=?1,lease_until=?2 WHERE id=?3",params![job.attempt,now()+60,job.id])?;}
            tx.commit()?; Ok(job)
        }).await
    }
    pub async fn finish_delivery(&self, id: &str, attempt: u32, success: bool) -> Result<()> {
        let id = id.to_owned();
        self.call(move |c| {
            let status=if success {"delivered"} else if attempt>=16 {"dead"} else {"pending"};
            let delay=(5_i64 * (1_i64 << attempt.min(10))).min(3600);
            c.execute("UPDATE outbox SET status=?1,due=?2,lease_until=0 WHERE id=?3 AND status='delivering' AND attempts=?4",params![status,now()+delay,id,attempt])?;
            Ok(())
        }).await
    }
    pub async fn retry_dead(&self, merchant_id: &str, id: &str) -> Result<Payment> {
        let (m, id) = (merchant_id.to_owned(), id.to_owned());
        self.call(move |c| {
            let data=c.query_row("SELECT data FROM payments WHERE id=?1 AND merchant_id=?2",params![id,m],|r|r.get(0)).optional()?.ok_or(Error::not_found())?;
            let changed=c.execute("UPDATE outbox SET status='pending',attempts=0,due=?1,lease_until=0 WHERE payment_id=?2 AND merchant_id=?3 AND status='dead'",params![now(),id,m])?;
            if changed==0 {return Err(Error::conflict("notification_not_dead"));}
            view(c,decode(data)?.payment)
        }).await
    }
}
