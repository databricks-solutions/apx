import os
from functools import cached_property

from databricks.sdk import WorkspaceClient
from databricks.sdk.errors import NotFound
from sqlalchemy import Engine, create_engine, event
from sqlmodel import SQLModel, Session, text

from .config import AppConfig
from .logger import logger


class Runtime:
    def __init__(self, config: AppConfig) -> None:
        self.config = config

    @cached_property
    def _dev_db_port(self) -> int | None:
        """Check for APX_DEV_DB_PORT environment variable for local development."""
        port = os.environ.get("APX_DEV_DB_PORT")
        return int(port) if port else None

    @cached_property
    def ws(self) -> WorkspaceClient:
        # note - this workspace client is usually an SP-based client
        # in development it usually uses the DATABRICKS_CONFIG_PROFILE
        return WorkspaceClient()

    @cached_property
    def engine_url(self) -> str:
        # Check if we're in local dev mode with APX_DEV_DB_PORT
        if self._dev_db_port:
            logger.info(f"Using local dev database at localhost:{self._dev_db_port}")
            username = "postgres"
            password = os.environ.get("APX_DEV_DB_PWD")
            if password is None:
                raise ValueError(
                    "APX server didn't provide a password, please check the dev server logs"
                )
            return f"postgresql+psycopg://{username}:{password}@localhost:{self._dev_db_port}/postgres?sslmode=disable"

        # Production mode: use Databricks Database
        logger.info(
            f"Using Databricks database instance: {self.config.db.instance_name}"
        )
        instance = self.ws.database.get_database_instance(self.config.db.instance_name)
        prefix = "postgresql+psycopg"
        host = instance.read_write_dns
        port = self.config.db.port
        database = self.config.db.database_name
        username = (
            self.ws.config.client_id
            if self.ws.config.client_id
            else self.ws.current_user.me().user_name
        )
        return f"{prefix}://{username}:@{host}:{port}/{database}"

    def _before_connect(self, dialect, conn_rec, cargs, cparams):
        cred = self.ws.database.generate_database_credential(
            instance_names=[self.config.db.instance_name]
        )
        cparams["password"] = cred.token

    @cached_property
    def engine(self) -> Engine:
        # In dev mode: no SSL, no password callback, single connection (PGlite limit)
        # In production: require SSL and use Databricks credential callback
        if self._dev_db_port:
            engine = create_engine(
                self.engine_url,
                pool_recycle=10,
                pool_size=4,
            )
        else:
            engine = create_engine(
                self.engine_url,
                pool_recycle=45 * 60,
                connect_args={"sslmode": "require"},
                pool_size=4,
            )
            event.listens_for(engine, "do_connect")(self._before_connect)
        return engine

    def get_session(self) -> Session:
        return Session(self.engine)

    def validate_db(self) -> None:
        # In dev mode, skip Databricks-specific validation
        if self._dev_db_port:
            logger.info(
                f"Validating local dev database connection at localhost:{self._dev_db_port}"
            )
        else:
            logger.info(
                f"Validating database connection to instance {self.config.db.instance_name}"
            )
            # check if the database instance exists
            try:
                self.ws.database.get_database_instance(self.config.db.instance_name)
            except NotFound:
                raise ValueError(
                    f"Database instance {self.config.db.instance_name} does not exist"
                )

        # check if a connection to the database can be established
        try:
            with self.get_session() as session:
                session.connection().execute(text("SELECT 1"))
                session.close()

        except Exception:
            raise ConnectionError("Failed to connect to the database")

        if self._dev_db_port:
            logger.info("Local dev database connection validated successfully")
        else:
            logger.info(
                f"Database connection to instance {self.config.db.instance_name} validated successfully"
            )

    def initialize_models(self) -> None:
        logger.info("Initializing database models")
        SQLModel.metadata.create_all(self.engine)
        logger.info("Database models initialized successfully")
